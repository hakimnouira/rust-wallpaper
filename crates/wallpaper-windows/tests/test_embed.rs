#[cfg(windows)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;
    use wallpaper_core::{HardwareAcceleration, PlaybackConfig};
    use wallpaper_windows::{
        desktop_host::ensure_desktop_host,
        mpv::{MpvLaunchConfig, MpvPlayer},
        window::WallpaperWindow,
    };
    use windows::Win32::Foundation::COLORREF;

    #[test]
    fn test_desktop_host_and_window_lifecycle() {
        let host = ensure_desktop_host().expect("failed to discover desktop host");
        assert!(host.is_valid(), "desktop host HWND must be valid");

        let bg_color = COLORREF(0x001F1A18);
        let win = WallpaperWindow::create(
            host.hwnd,
            "TEST_DISPLAY".to_string(),
            0,
            0,
            640,
            480,
            true,
            bg_color,
        )
        .expect("failed to create wallpaper window");

        // Test creating a child window directly inside win.hwnd using CreateWindowExW
        unsafe {
            use windows::core::{w, PCWSTR};
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WS_CHILD, WS_EX_NOPARENTNOTIFY, WS_VISIBLE,
            };

            let hinst = GetModuleHandleW(PCWSTR::null()).unwrap();
            let child_res = CreateWindowExW(
                WS_EX_NOPARENTNOTIFY,
                w!("STATIC"),
                w!("test_static"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                300,
                200,
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinst,
                None,
            );
            eprintln!("Child window result: {:?}", child_res);
            let child = child_res.expect("Child window creation failed");
            let _ = DestroyWindow(child);
        }

        use windows::Win32::UI::WindowsAndMessaging::{WS_CHILD, WS_POPUP};

        // Initially created as a top-level popup
        let style_init = win.get_style();
        assert_ne!(
            style_init & WS_POPUP.0,
            0,
            "window must start with WS_POPUP style"
        );
        assert_eq!(
            style_init & WS_CHILD.0,
            0,
            "window must not have WS_CHILD initially"
        );

        // Attach to WorkerW host: clear WS_POPUP, set WS_CHILD
        win.attach_to_host().expect("attach_to_host must succeed");
        let parent_attached = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        assert_eq!(
            parent_attached, host.hwnd,
            "window parent must be host HWND when attached"
        );
        let style_attached = win.get_style();
        assert_ne!(
            style_attached & WS_CHILD.0,
            0,
            "window must have WS_CHILD style when attached to WorkerW"
        );
        assert_eq!(
            style_attached & WS_POPUP.0,
            0,
            "window must clear WS_POPUP style when attached to WorkerW"
        );

        // Detach to unowned popup: clear WS_CHILD, set WS_POPUP
        win.detach_from_host()
            .expect("detach_from_host must succeed");
        let parent_detached = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        assert_ne!(
            parent_detached, host.hwnd,
            "window parent must not be host HWND when detached"
        );
        let style_detached = win.get_style();
        assert_ne!(
            style_detached & WS_POPUP.0,
            0,
            "window must restore WS_POPUP style when detached"
        );
        assert_eq!(
            style_detached & WS_CHILD.0,
            0,
            "window must clear WS_CHILD style when detached"
        );

        // Re-attach to WorkerW host
        win.attach_to_host().expect("re-attach must succeed");
        let parent_reattach = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        assert_eq!(
            parent_reattach, host.hwnd,
            "window parent must be host HWND after re-attaching"
        );
        let style_reattached = win.get_style();
        assert_ne!(
            style_reattached & WS_CHILD.0,
            0,
            "window must have WS_CHILD style after re-attaching"
        );
        assert_eq!(
            style_reattached & WS_POPUP.0,
            0,
            "window must clear WS_POPUP style after re-attaching"
        );
    }

    #[test]
    fn test_mpv_embedding_and_hardware_decoding() {
        let mut video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            video_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }
        if !video_path.exists() {
            panic!("sample-nature.mp4 not found at {:?}", video_path);
        }

        let host = ensure_desktop_host().expect("failed to discover desktop host");
        let win = WallpaperWindow::create(
            host.hwnd,
            "TEST_EMBED".to_string(),
            100,
            100,
            800,
            600,
            false,
            COLORREF(0),
        )
        .expect("failed to create window");

        let cfg = PlaybackConfig {
            loop_playback: true,
            mute: true,
            volume: 0,
            hardware_acceleration: HardwareAcceleration::Auto,
        };

        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{
                GetAncestor, GetWindowLongPtrW, GetWindowRect, IsWindow, GA_PARENT, GWL_EXSTYLE,
                GWL_STYLE,
            };
            let is_win = IsWindow(win.hwnd).as_bool();
            let style = GetWindowLongPtrW(win.hwnd, GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(win.hwnd, GWL_EXSTYLE) as u32;
            let parent = GetAncestor(win.hwnd, GA_PARENT);
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = GetWindowRect(win.hwnd, &mut r);
            println!(
                "TARGET HWND: 0x{:X} ({}), IsWindow: {}, Style: 0x{:08X}, ExStyle: 0x{:08X}, Parent: 0x{:X}, Rect: ({}, {}, {}, {})",
                win.hwnd.0 as usize, win.hwnd.0 as usize, is_win, style, ex_style, parent.0 as usize,
                r.left, r.top, r.right, r.bottom
            );
        }

        let mpv_path = wallpaper_windows::find_mpv_executable().expect("mpv executable");
        println!("TEST MPV PATH: {:?}", mpv_path);

        let launch_cfg = MpvLaunchConfig {
            video_path,
            wid: win.hwnd.0 as isize,
            config: cfg,
        };

        println!("STEP 1: launching player");
        let mut player = MpvPlayer::launch(launch_cfg).expect("failed to launch embedded MPV");
        println!("STEP 2: player launched, alive={}", player.is_alive());
        assert!(player.is_alive(), "player process must be alive");

        // Wait 1 second and check decoder/playback first
        std::thread::sleep(Duration::from_millis(1000));
        let decoder = player.query_active_decoder();
        let time_pos = player.query_time_pos();
        println!("DEBUG: decoder = {:?}, time_pos = {:?}", decoder, time_pos);

        // Enumerate all windows belonging to player's process or children of win.hwnd
        unsafe {
            use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
            use windows::Win32::UI::WindowsAndMessaging::{
                EnumChildWindows, EnumWindows, GetWindowThreadProcessId,
            };

            unsafe extern "system" fn enum_child(hwnd: HWND, _lparam: LPARAM) -> BOOL {
                let class = WallpaperWindow::get_window_class(hwnd);
                println!(
                    "ENUM_CHILD of win.hwnd: HWND = 0x{:X}, class = {:?}",
                    hwnd.0 as usize, class
                );
                BOOL(1)
            }
            let _ = EnumChildWindows(win.hwnd, Some(enum_child), LPARAM(0));

            let pid = player.child_id();
            println!("MPV PID = {}", pid);
            unsafe extern "system" fn enum_win(hwnd: HWND, lparam: LPARAM) -> BOOL {
                let mut win_pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
                if win_pid == lparam.0 as u32 {
                    let class = WallpaperWindow::get_window_class(hwnd);
                    let parent = windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                        hwnd,
                        windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
                    );
                    println!(
                        "ENUM_WIN of MPV pid: HWND = 0x{:X}, class = {:?}, parent = 0x{:X}",
                        hwnd.0 as usize, class, parent.0 as usize
                    );
                }
                BOOL(1)
            }
            let _ = EnumWindows(Some(enum_win), LPARAM(pid as isize));
        }

        let child_hwnd = win.find_child_hwnd();
        let child_class = child_hwnd.and_then(WallpaperWindow::get_window_class);
        println!(
            "EVIDENCE: MPV child HWND = {:?}, class = {:?}",
            child_hwnd, child_class
        );
        let child_hwnd =
            child_hwnd.expect("MPV child rendering HWND must exist inside wallpaper window");
        assert_eq!(child_class.as_deref(), Some("mpv"));

        // 2. Verify child window belongs to expected parent hierarchy
        let child_parent = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                child_hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        println!(
            "EVIDENCE: Child's parent HWND = 0x{:X}, expected wallpaper HWND = 0x{:X}",
            child_parent.0 as usize, win.hwnd.0 as usize
        );
        assert_eq!(
            child_parent, win.hwnd,
            "child parent must match wallpaper HWND"
        );

        // 3. Attach to desktop host and verify WorkerW hierarchy & style transitions
        println!("STEP 3: attaching wallpaper window to WorkerW host");
        let style_before_attach = win.get_style();
        assert_ne!(
            style_before_attach & windows::Win32::UI::WindowsAndMessaging::WS_POPUP.0,
            0,
            "wallpaper window must have WS_POPUP before attaching"
        );
        assert_eq!(
            style_before_attach & windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0,
            0,
            "wallpaper window must not have WS_CHILD before attaching"
        );

        win.attach_to_host().expect("attach_to_host must succeed");

        let style_after_attach = win.get_style();
        assert_ne!(
            style_after_attach & windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0,
            0,
            "wallpaper window must have WS_CHILD after attaching to WorkerW"
        );
        assert_eq!(
            style_after_attach & windows::Win32::UI::WindowsAndMessaging::WS_POPUP.0,
            0,
            "wallpaper window must clear WS_POPUP after attaching to WorkerW"
        );

        // Verify MPV child window remains valid and parented under win.hwnd after attachment
        let child_after_attach = win
            .find_child_hwnd()
            .expect("MPV child HWND must remain valid after attach");
        assert_eq!(
            child_after_attach, child_hwnd,
            "child HWND handle must match"
        );
        let child_parent_after = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                child_after_attach,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        assert_eq!(
            child_parent_after, win.hwnd,
            "child parent must still be wallpaper HWND"
        );

        let win_parent = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        println!(
            "EVIDENCE: Wallpaper parent HWND = 0x{:X}, expected WorkerW HWND = 0x{:X}",
            win_parent.0 as usize, host.hwnd.0 as usize
        );
        assert_eq!(
            win_parent, host.hwnd,
            "wallpaper window must be parented under WorkerW"
        );

        std::thread::sleep(Duration::from_millis(500));
        assert!(
            player.is_alive(),
            "player must remain alive after reparenting to WorkerW"
        );

        // 4 & 5. Query decoder & distinguish hardware acceleration from software fallback
        let active_decoder = player
            .query_active_decoder()
            .expect("query_active_decoder must succeed");
        let is_hw = player.is_hardware_decoding_active();
        let (w, h, fps, gpu) = player.query_video_details();
        println!(
            "EVIDENCE: active_decoder = {}, is_hardware = {}, gpu_context = {:?}, dimensions = {:?}x{:?}, fps = {:?}",
            active_decoder, is_hw, gpu, w, h, fps
        );
        assert!(
            is_hw,
            "Hardware decoding must be genuinely active, got {}",
            active_decoder
        );
        assert_eq!(
            active_decoder, "d3d11va",
            "Hardware decoder must be d3d11va on Direct3D 11"
        );
        assert!(w.unwrap_or(0) > 0, "Video width must be positive");
        assert!(h.unwrap_or(0) > 0, "Video height must be positive");

        // 6. Verify video time-pos advances
        let t1 = player.query_time_pos().expect("initial time-pos");
        std::thread::sleep(Duration::from_millis(300));
        let t2 = player.query_time_pos().expect("time-pos after 300ms");
        println!(
            "EVIDENCE: Playing time-pos progression: t1 = {:.3}s, t2 = {:.3}s",
            t1, t2
        );
        assert!(
            t2 > t1,
            "time-pos must advance during playback (t1={t1}, t2={t2})"
        );

        // 7. Verify pause freezes time-pos
        println!("STEP 5: testing pause");
        player.pause().expect("failed to pause");
        std::thread::sleep(Duration::from_millis(50));
        let p1 = player.query_time_pos().expect("time-pos after pause");
        std::thread::sleep(Duration::from_millis(300));
        let p2 = player
            .query_time_pos()
            .expect("time-pos after 300ms while paused");
        println!(
            "EVIDENCE: Paused time-pos stability: p1 = {:.3}s, p2 = {:.3}s (delta = {:.4}s)",
            p1,
            p2,
            (p2 - p1).abs()
        );
        assert!(
            (p2 - p1).abs() < 0.05,
            "time-pos must remain frozen during pause (p1={p1}, p2={p2})"
        );

        // 8. Verify resume advances time-pos again
        println!("STEP 6: testing resume");
        player.resume().expect("failed to resume");
        std::thread::sleep(Duration::from_millis(400));
        let r1 = player
            .query_time_pos()
            .expect("time-pos after resume and playback");
        println!(
            "EVIDENCE: Resumed time-pos advancement: p2 = {:.3}s, r1 = {:.3}s",
            p2, r1
        );
        assert!(
            r1 > p2 + 0.1,
            "time-pos must advance again after resume (p2={p2}, r1={r1})"
        );

        // 9. Clean shutdown
        println!("STEP 7: testing stop");
        player.stop().expect("failed to stop player");
        println!("STEP 8: finished stop");
        assert!(!player.is_alive(), "player must be stopped");
    }

    #[test]
    fn test_ipc_stress_and_rapid_commands() {
        let mut video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            video_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }
        let host = ensure_desktop_host().expect("failed to discover desktop host");
        let win = WallpaperWindow::create(
            host.hwnd,
            "TEST_STRESS".to_string(),
            0,
            0,
            400,
            300,
            false,
            COLORREF(0),
        )
        .expect("create window");

        let mpv_path = wallpaper_windows::find_mpv_executable().expect("mpv executable");
        println!("TEST MPV PATH: {:?}", mpv_path);

        let launch_cfg = MpvLaunchConfig {
            video_path,
            wid: win.hwnd.0 as isize,
            config: PlaybackConfig::default(),
        };

        let mut player = MpvPlayer::launch(launch_cfg).expect("launch player");

        // Rapidly send 50 commands without blocking
        for i in 0..50 {
            let volume = (i % 100) as u32;
            player.set_volume(volume).expect("set_volume");
            let _ = player.query_active_decoder();
        }

        // Test shutdown while requests are pending
        let (tx_pending, rx_pending) = std::sync::mpsc::channel();
        player.insert_mock_pending_request(999_999, tx_pending);

        // player.stop() must complete pending requests with errors and clean up child
        player.stop().expect("stop player");
        assert!(!player.is_alive(), "player must stop cleanly");

        let pending_res = rx_pending
            .recv_timeout(Duration::from_millis(500))
            .expect("pending request must receive response upon stop");
        assert!(
            pending_res.is_err(),
            "pending request must resolve to an Err upon player stop"
        );
    }

    #[test]
    fn test_ipc_writer_failure_and_pending_cleanup() {
        let mut video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            video_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }
        let host = ensure_desktop_host().expect("failed to discover desktop host");
        let win = WallpaperWindow::create(
            host.hwnd,
            "TEST_FAIL_CLEANUP".to_string(),
            0,
            0,
            400,
            300,
            false,
            COLORREF(0),
        )
        .expect("create window");

        let launch_cfg = MpvLaunchConfig {
            video_path,
            wid: win.hwnd.0 as isize,
            config: PlaybackConfig::default(),
        };

        let mut player = MpvPlayer::launch(launch_cfg).expect("launch player");
        assert_eq!(player.pending_request_count(), 0);

        // 1. Verify normal command execution leaves 0 pending entries
        player.set_volume(50).expect("normal command succeeds");
        assert_eq!(player.pending_request_count(), 0);

        // 2. Simulate writer failure / disconnect
        player.disconnect_pipe_writer_for_test();
        let fail_res = player.set_volume(75);
        assert!(
            fail_res.is_err(),
            "command must fail when writer is disconnected"
        );

        // 3. Ensure pending_responses entry was cleaned up and not leaked
        assert_eq!(
            player.pending_request_count(),
            0,
            "failed command must not leave lingering entries in pending_responses"
        );

        // 4. Repeated stop calls must be safe and idempotent
        player.stop().expect("first stop must succeed");
        assert!(!player.is_alive());
        player
            .stop()
            .expect("second stop must succeed idempotently");
        player.stop().expect("third stop must succeed idempotently");
    }

    #[test]
    fn test_manager_missing_file_validation_preserves_existing_player() {
        use wallpaper_windows::manager::{WallpaperManager, WallpaperManagerError};

        let mut manager = WallpaperManager::init().expect("initialize wallpaper manager");
        let devices = manager.active_devices();
        assert!(
            !devices.is_empty(),
            "must have at least one active monitor device"
        );

        let primary_device = devices[0].clone();

        let mut valid_video = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !valid_video.exists() {
            valid_video = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }

        // 1. Start a valid video player on primary monitor
        manager
            .play_video_on_monitor(&primary_device, valid_video, PlaybackConfig::default())
            .expect("initial playback must succeed");
        assert_eq!(manager.player_count(), 1, "must have 1 active player");

        // 2. Attempt to play a non-existent file on the same monitor
        let missing_video = PathBuf::from("assets/definitely_missing_wallpaper_12345.mp4");
        let res = manager.play_video_on_monitor(
            &primary_device,
            missing_video,
            PlaybackConfig::default(),
        );

        // Must fail with VideoFileInaccessible
        assert!(
            matches!(
                res,
                Err(WallpaperManagerError::VideoFileInaccessible { .. })
            ),
            "must return VideoFileInaccessible error"
        );

        // 3. Verify manager state was PRESERVED: existing player was NOT stopped or removed
        assert_eq!(
            manager.player_count(),
            1,
            "existing player must not be removed when new video path is invalid"
        );
        let statuses = manager.player_statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].0, primary_device);

        // 4. Verify window state was preserved and was never left detached
        let win = manager
            .window(&primary_device)
            .expect("wallpaper window must still exist");
        let style = win.get_style();
        assert_ne!(
            style & windows::Win32::UI::WindowsAndMessaging::WS_CHILD.0,
            0,
            "window must remain WS_CHILD attached to host"
        );
        assert_eq!(
            style & windows::Win32::UI::WindowsAndMessaging::WS_POPUP.0,
            0,
            "window must not have WS_POPUP"
        );

        // 5. Clean shutdown
        manager.stop_all();
        assert_eq!(manager.player_count(), 0);
        manager.stop_all(); // Idempotent stop_all
    }

    #[test]
    fn test_manager_stop_all_preserves_failed_player_tracking() {
        use wallpaper_windows::manager::WallpaperManager;

        let mut manager = WallpaperManager::init().expect("initialize wallpaper manager");
        let devices = manager.active_devices();
        assert!(
            !devices.is_empty(),
            "must have at least one active monitor device"
        );

        let primary_device = devices[0].clone();

        let mut valid_video = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !valid_video.exists() {
            valid_video = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }

        // 1. Start video playback on primary monitor
        manager
            .play_video_on_monitor(&primary_device, valid_video, PlaybackConfig::default())
            .expect("initial playback must succeed");
        assert_eq!(manager.player_count(), 1, "must have 1 active player");
        assert!(
            manager.has_active_source(&primary_device),
            "must have active source registered"
        );

        // 2. Inject mock stop failure on the player
        manager
            .player_mut(&primary_device)
            .expect("player must exist")
            .set_mock_stop_failure(true);

        // 3. Execute stop_all(): must attempt stop, retain failed player, clear active_sources
        let summary = manager.stop_all();
        assert!(
            !summary.is_success(),
            "stop_all must report failure when child stop fails"
        );
        assert_eq!(summary.stopped.len(), 0, "no players succeeded stopping");
        assert_eq!(summary.failed.len(), 1, "exactly 1 player failed stopping");
        assert_eq!(summary.failed[0].0, primary_device);

        // 4. Verify player is RETAINED in self.players and remains observable
        assert_eq!(
            manager.player_count(),
            1,
            "failed player must remain tracked in self.players"
        );
        assert!(
            manager.player(&primary_device).is_some(),
            "failed player must be observable in manager"
        );
        assert_eq!(
            manager.player_statuses().len(),
            1,
            "retained player must be present in player_statuses"
        );

        // 5. Verify active_sources was cleared, preventing unintended automatic restarts
        assert_eq!(
            manager.active_source_count(),
            0,
            "active sources must be cleared by global stop request"
        );
        assert!(
            !manager.has_active_source(&primary_device),
            "device must no longer have active source"
        );

        // Verify check_and_recover_players() does not restart or duplicate players
        manager.check_and_recover_players();
        assert_eq!(
            manager.player_count(),
            1,
            "retained player count must remain 1 after recovery check"
        );
        assert_eq!(manager.active_source_count(), 0);

        // 6. Reset mock failure and retry shutdown: must succeed and remove player
        manager
            .player_mut(&primary_device)
            .expect("player exists")
            .set_mock_stop_failure(false);

        let retry_summary = manager.stop_all();
        assert!(
            retry_summary.is_success(),
            "stop_all must succeed once mock failure is cleared"
        );
        assert_eq!(retry_summary.stopped.len(), 1);
        assert_eq!(retry_summary.stopped[0], primary_device);
        assert_eq!(retry_summary.failed.len(), 0);

        // Player is now confirmed terminated and removed from tracking
        assert_eq!(
            manager.player_count(),
            0,
            "player must be removed from self.players after successful termination"
        );
        assert!(manager.player(&primary_device).is_none());
    }

    #[test]
    fn test_explorer_recovery_all_players_succeed() {
        use wallpaper_windows::manager::WallpaperManager;

        let mut manager = WallpaperManager::init().expect("initialize wallpaper manager");
        let devices = manager.active_devices();
        assert!(!devices.is_empty(), "must have active monitor device");
        let primary_device = devices[0].clone();

        let mut valid_video = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !valid_video.exists() {
            valid_video = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }

        // 1. Start playback on primary monitor
        manager
            .play_video_on_monitor(&primary_device, valid_video, PlaybackConfig::default())
            .expect("initial playback must succeed");
        assert_eq!(manager.player_count(), 1);
        let initial_pid = manager.player(&primary_device).unwrap().child_id();

        // 2. Execute Explorer recovery: all players stop and restart successfully
        let res = manager.force_recover_explorer();
        assert!(
            res.is_ok(),
            "Explorer recovery must succeed when all players stop cleanly: {res:?}"
        );

        // 3. Verify clean state after recovery
        assert_eq!(manager.player_count(), 1);
        assert!(manager.has_active_source(&primary_device));
        let new_pid = {
            let player = manager
                .player_mut(&primary_device)
                .expect("player must exist");
            assert!(player.is_alive());
            player.child_id()
        };
        assert_ne!(
            new_pid, initial_pid,
            "replacement player must be a fresh process"
        );

        // 4. Teardown
        manager.stop_all();
        assert_eq!(manager.player_count(), 0);
    }

    #[test]
    fn test_explorer_recovery_partial_failure_preserves_failed_player_and_retries() {
        use wallpaper_windows::manager::{WallpaperManager, WallpaperManagerError};
        use wallpaper_windows::MonitorInfo;

        let mut manager = WallpaperManager::init().expect("initialize wallpaper manager");
        let mut devices = manager.active_devices();

        // If fewer than 2 real monitors, configure deterministic simulated topology
        if devices.len() < 2 {
            let test_monitors = vec![
                MonitorInfo {
                    device_name: "\\\\.\\DISPLAY1".to_string(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    is_primary: true,
                },
                MonitorInfo {
                    device_name: "\\\\.\\DISPLAY2".to_string(),
                    x: 1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    is_primary: false,
                },
            ];
            manager.set_test_monitors(Some(test_monitors));
            manager.refresh().expect("synchronize test monitors");
            devices = manager.active_devices();
        }

        assert!(devices.len() >= 2, "test requires at least two monitors");
        let dev1 = devices[0].clone();
        let dev2 = devices[1].clone();

        let mut valid_video = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !valid_video.exists() {
            valid_video = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../assets/wallpapers/sample-nature.mp4");
        }

        // 1. Launch players on both monitors
        manager
            .play_video_on_monitor(&dev1, valid_video.clone(), PlaybackConfig::default())
            .expect("play dev1 must succeed");
        manager
            .play_video_on_monitor(&dev2, valid_video.clone(), PlaybackConfig::default())
            .expect("play dev2 must succeed");
        assert_eq!(manager.player_count(), 2);

        let pid1_initial = manager.player(&dev1).unwrap().child_id();
        let pid2_initial = manager.player(&dev2).unwrap().child_id();

        // 2. Inject stop failure on dev2 only
        manager
            .player_mut(&dev2)
            .expect("dev2 player must exist")
            .set_mock_stop_failure(true);

        // 3. Execute Explorer recovery: dev1 succeeds, dev2 fails
        let res = manager.force_recover_explorer();

        // 4. Verify recovery failure is exposed clearly
        match res {
            Err(WallpaperManagerError::ExplorerRecoveryPartial {
                failed_monitors, ..
            }) => {
                assert!(
                    failed_monitors.contains(&dev2),
                    "failed_monitors must report dev2"
                );
                assert!(
                    !failed_monitors.contains(&dev1),
                    "failed_monitors must not report dev1"
                );
            }
            other => panic!("expected ExplorerRecoveryPartial error, got {other:?}"),
        }

        // 5. Verify dev1 was recovered independently with a new player
        let pid1_new = {
            let p1 = manager.player_mut(&dev1).expect("dev1 player must exist");
            assert!(p1.is_alive());
            p1.child_id()
        };
        assert_ne!(
            pid1_new, pid1_initial,
            "dev1 must have a new replacement player process"
        );
        assert!(manager.has_active_source(&dev1));

        // 6. Verify dev2 failed player is RETAINED and NO duplicate player was created
        let p2 = manager
            .player(&dev2)
            .expect("dev2 player must be retained in manager");
        assert_eq!(
            p2.child_id(),
            pid2_initial,
            "dev2 must retain the original player process, no duplicate process created"
        );
        assert!(
            manager.has_active_source(&dev2),
            "dev2 saved source must be preserved for future recovery"
        );
        assert_eq!(
            manager.player_count(),
            2,
            "total player count must remain exactly 2 (1 new for dev1, 1 retained for dev2)"
        );

        // 7. Verify check_and_recover_players() does not launch a duplicate player for dev2
        manager.check_and_recover_players();
        assert_eq!(manager.player_count(), 2);
        assert_eq!(manager.player(&dev2).unwrap().child_id(), pid2_initial);

        // 8. Test subsequent retry: reset mock failure and re-run recovery
        manager
            .player_mut(&dev2)
            .expect("dev2 player exists")
            .set_mock_stop_failure(false);

        let retry_res = manager.force_recover_explorer();
        assert!(
            retry_res.is_ok(),
            "Explorer recovery retry must succeed once failure condition is cleared: {retry_res:?}"
        );

        // 9. Both monitors are now recovered with fresh players
        assert_eq!(manager.player_count(), 2);
        assert!(manager.player_mut(&dev1).unwrap().is_alive());
        let p2_final = manager.player_mut(&dev2).unwrap();
        assert!(p2_final.is_alive());
        let p2_final_pid = p2_final.child_id();
        assert_ne!(
            p2_final_pid, pid2_initial,
            "dev2 must now have a replacement player process after successful retry"
        );
        assert!(manager.has_active_source(&dev1));
        assert!(manager.has_active_source(&dev2));

        // 10. Clean teardown
        manager.stop_all();
        assert_eq!(manager.player_count(), 0);
    }

    #[test]
    fn test_wallpaper_window_safe_drop_and_stale_cleanup() {
        use windows::core::w;
        use windows::Win32::Foundation::COLORREF;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, IsWindow, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };

        let host = ensure_desktop_host().expect("host");

        // 1. Dropping a valid wallpaper window
        {
            let win = WallpaperWindow::create(
                host.hwnd,
                "TEST_DROP_VALID".to_string(),
                0,
                0,
                300,
                200,
                false,
                COLORREF(0),
            )
            .expect("create window");
            let hwnd = win.hwnd;
            assert!(win.is_valid());
            drop(win);
            assert!(!unsafe { IsWindow(hwnd).as_bool() });
        }

        // 2. Dropping a window whose HWND has already been destroyed
        {
            let win = WallpaperWindow::create(
                host.hwnd,
                "TEST_DROP_ALREADY_DESTROYED".to_string(),
                0,
                0,
                300,
                200,
                false,
                COLORREF(0),
            )
            .expect("create window");
            let hwnd = win.hwnd;
            assert!(win.is_valid());

            unsafe {
                let res = DestroyWindow(hwnd);
                assert!(res.is_ok(), "direct DestroyWindow must succeed");
            }
            assert!(!win.is_valid(), "window must report invalid before drop");
            drop(win); // Must not panic or double-destroy
        }

        // 3. Dropping a window after its host HWND has been destroyed
        {
            let dummy_host = unsafe {
                CreateWindowExW(
                    Default::default(),
                    w!("STATIC"),
                    w!("DummyHostWindow"),
                    WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                    100,
                    100,
                    400,
                    300,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("create dummy host window")
            };
            assert!(unsafe { IsWindow(dummy_host).as_bool() });

            let win = WallpaperWindow::create(
                dummy_host,
                "TEST_DROP_HOST_DESTROYED".to_string(),
                0,
                0,
                200,
                150,
                false,
                COLORREF(0),
            )
            .expect("create child window parented to dummy host");
            win.attach_to_host().expect("attach to dummy host");
            let child_hwnd = win.hwnd;
            assert!(win.is_valid());

            // Destroy the host window: cascades destruction to child in Win32
            unsafe {
                let res = DestroyWindow(dummy_host);
                assert!(res.is_ok(), "destroying dummy host must succeed");
            }
            assert!(!unsafe { IsWindow(dummy_host).as_bool() });
            assert!(!unsafe { IsWindow(child_hwnd).as_bool() });
            assert!(!win.is_valid());

            drop(win); // Must safely handle both child and host HWND already dead
        }

        // 4. Repeated cleanup without panic or double-free
        {
            let mut win = WallpaperWindow::create(
                host.hwnd,
                "TEST_REPEAT_CLEANUP".to_string(),
                0,
                0,
                300,
                200,
                false,
                COLORREF(0),
            )
            .expect("create window");
            let hwnd = win.hwnd;
            assert!(win.is_valid());

            // First manual cleanup
            win.destroy();
            assert!(!win.is_valid());
            assert!(!unsafe { IsWindow(hwnd).as_bool() });

            // Second manual cleanup (idempotent, must not double-free brush or fail)
            win.destroy();

            // Third cleanup via Drop (must not panic or double-free)
            drop(win);
        }

        // 5. Manager detects and recreates stale window upon refresh
        {
            let mut manager =
                wallpaper_windows::manager::WallpaperManager::init().expect("init manager");
            let devices = manager.active_devices();
            let dev = devices[0].clone();
            let orig_hwnd = manager.window(&dev).unwrap().hwnd;

            // Invalidate the HWND externally
            unsafe {
                let _ = DestroyWindow(orig_hwnd);
            }
            assert!(!manager.window(&dev).unwrap().is_valid());

            // Calling refresh() must detect the stale window and recreate a fresh valid window
            manager.refresh().expect("refresh must handle stale window");
            let new_hwnd = manager.window(&dev).unwrap().hwnd;
            assert_ne!(orig_hwnd, new_hwnd);
            assert!(manager.window(&dev).unwrap().is_valid());

            manager.stop_all();
        }
    }
}
