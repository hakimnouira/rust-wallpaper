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
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWL_STYLE, WS_POPUP};

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

        // Initially attached to WorkerW host
        win.attach_to_host();
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
        let style_attached = unsafe { GetWindowLongPtrW(win.hwnd, GWL_STYLE) as u32 };
        assert_ne!(
            style_attached & WS_POPUP.0,
            0,
            "window must preserve WS_POPUP for cross-process embedding"
        );

        // Detach to unowned popup
        win.detach_from_host();
        let parent_detached = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetAncestor(
                win.hwnd,
                windows::Win32::UI::WindowsAndMessaging::GA_PARENT,
            )
        };
        // Desktop / unowned parent is either desktop window or default
        assert_ne!(
            parent_detached, host.hwnd,
            "window parent must not be host HWND when detached"
        );

        // Re-attach
        win.attach_to_host();
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

        // 3. Attach to desktop host and verify WorkerW hierarchy
        println!("STEP 3: attaching wallpaper window to WorkerW host");
        win.attach_to_host();

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

        // Test shutdown with pending requests
        player.stop().expect("stop player");
        assert!(!player.is_alive());
    }
}
