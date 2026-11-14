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

        let launch_cfg = MpvLaunchConfig {
            video_path,
            wid: win.hwnd.0 as isize,
            config: cfg,
        };

        println!("STEP 1: launching player");
        let mut player = MpvPlayer::launch(launch_cfg).expect("failed to launch embedded MPV");
        println!("STEP 2: player launched, alive={}", player.is_alive());
        assert!(player.is_alive(), "player process must be alive");

        // Re-attach to host behind icons
        println!("STEP 3: re-attaching to host");
        win.attach_to_host();

        println!("STEP 4: sleeping 500ms");
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            player.is_alive(),
            "player must remain alive after reparenting to WorkerW"
        );

        // Test pause and resume
        println!("STEP 5: testing pause");
        player.pause().expect("failed to pause");
        println!("STEP 6: testing resume");
        std::thread::sleep(Duration::from_millis(100));
        player.resume().expect("failed to resume");

        // Clean shutdown
        println!("STEP 7: testing stop");
        player.stop().expect("failed to stop player");
        println!("STEP 8: finished stop");
        assert!(!player.is_alive(), "player must be stopped");
    }
}
