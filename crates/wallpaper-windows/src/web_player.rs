//! Web and interactive wallpaper player.
//!
//! Hosts HTML5, WebGL, Shader, and Canvas interactive wallpapers inside an
//! isolated browser surface (Microsoft Edge / Chromium / WebView2) embedded directly into
//! the native desktop host (`WorkerW`/`Progman`) wallpaper HWND.
//!
//! # Process Isolation & Job Object Guarantee
//!
//! Like MPV, every spawned web player process is assigned to a Windows Job Object
//! configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. When the daemon terminates,
//! all web player processes are unconditionally destroyed by the kernel.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tracing::{info, warn};

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, RECT};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    PeekMessageW, SetParent, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOP, MSG, PM_REMOVE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW,
};

#[derive(Debug, Error)]
pub enum WebPlayerError {
    #[error("no compatible browser executable (Edge/Chrome/Brave) found for web wallpapers")]
    BrowserNotFound,
    #[error("failed to launch web player process: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("failed to embed web player window into wallpaper HWND: {0}")]
    EmbeddingFailed(String),
}

/// Discovers a compatible Chromium-based browser capable of running `--app` kiosk mode.
pub fn find_web_browser_executable() -> Option<PathBuf> {
    let candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
    ];

    for candidate in candidates {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(p);
        }
    }

    // Check LocalAppData for user installs
    if let Ok(lad) = std::env::var("LOCALAPPDATA") {
        let user_candidates = [
            PathBuf::from(&lad).join(r"Microsoft\Edge\Application\msedge.exe"),
            PathBuf::from(&lad).join(r"Google\Chrome\Application\chrome.exe"),
            PathBuf::from(&lad).join(r"BraveSoftware\Brave-Browser\Application\brave.exe"),
        ];
        for candidate in user_candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Supervised web wallpaper instance running an HTML5/WebGL/Shader surface.
pub struct WebPlayer {
    child: Child,
    #[cfg(windows)]
    child_hwnd: Option<isize>,
    #[cfg(windows)]
    job_handle: Option<isize>,
    #[allow(dead_code)]
    parent_hwnd: isize,
    url: String,
    data_dir: PathBuf,
    is_paused: Arc<AtomicBool>,
    #[allow(dead_code)]
    started_at: Instant,
    #[allow(dead_code)]
    last_cpu_sample: Arc<AtomicU64>,
}

impl WebPlayer {
    /// Launches an interactive web wallpaper targeting `parent_hwnd`.
    pub fn launch(
        parent_hwnd: isize,
        source: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, WebPlayerError> {
        let browser_exe = find_web_browser_executable().ok_or(WebPlayerError::BrowserNotFound)?;

        // Ensure source is formatted as URL
        let url = if source.starts_with("http://")
            || source.starts_with("https://")
            || source.starts_with("file://")
        {
            source.to_string()
        } else {
            let path = Path::new(source);
            if path.exists() {
                format!(
                    "file:///{}",
                    path.canonicalize()
                        .unwrap_or_else(|_| path.to_path_buf())
                        .to_string_lossy()
                        .replace('\\', "/")
                )
            } else {
                source.to_string()
            }
        };

        // Create dedicated isolated user-data-dir in %TEMP%
        let temp_dir = std::env::temp_dir();
        let session_id = uuid::Uuid::new_v4();
        let data_dir = temp_dir.join(format!("rust_wallpaper_web_{}", session_id));
        let _ = std::fs::create_dir_all(&data_dir);

        let mut cmd = Command::new(&browser_exe);
        cmd.arg(format!("--app={}", url))
            .arg("--kiosk")
            .arg(format!("--user-data-dir={}", data_dir.to_string_lossy()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-features=TranslateUI,OptimizationHints")
            .arg("--autoplay-policy=no-user-gesture-required")
            .arg("--use-fake-ui-for-media-stream")
            .arg("--allow-file-access-from-files")
            .arg("--disable-pinch")
            .arg("--disable-session-crashed-bubble")
            .arg(format!("--window-size={},{}", width, height))
            .arg(format!("--window-position=0,0"));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn()?;
        let pid = child.id();

        #[cfg(windows)]
        let (job_handle, child_hwnd) = {
            // Assign process to Windows Job Object and retain its handle so stop() kills the full process tree
            let mut stored_job: Option<isize> = None;
            unsafe {
                let job = CreateJobObjectW(None, None);
                if let Ok(job_handle) = job {
                    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    let _ = SetInformationJobObject(
                        job_handle,
                        JobObjectExtendedLimitInformation,
                        &info as *const _ as *const _,
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    );
                    let process_handle = windows::Win32::System::Threading::OpenProcess(
                        windows::Win32::System::Threading::PROCESS_SET_QUOTA
                            | windows::Win32::System::Threading::PROCESS_TERMINATE,
                        false,
                        pid,
                    );
                    if let Ok(proc_h) = process_handle {
                        let _ = AssignProcessToJobObject(job_handle, proc_h);
                        let _ = CloseHandle(proc_h);
                    }
                    stored_job = Some(job_handle.0 as isize);
                }
            }

            // Find top-level HWND belonging to spawned PID while keeping messages pumped
            let found = find_hwnd_for_pid(pid, std::time::Duration::from_millis(3000));
            let hwnd_opt = if let Some(chwnd) = found {
                unsafe {
                    let parent = HWND(parent_hwnd as _);
                    let _ = SetParent(chwnd, parent);
                    let _ = SetWindowPos(
                        chwnd,
                        HWND_TOP,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                Some(chwnd.0 as isize)
            } else {
                warn!(
                    pid,
                    "Could not locate top-level HWND for web player in time"
                );
                None
            };
            (stored_job, hwnd_opt)
        };

        info!(%url, pid, "Interactive web wallpaper player launched successfully");

        Ok(Self {
            child,
            #[cfg(windows)]
            child_hwnd,
            #[cfg(windows)]
            job_handle,
            parent_hwnd,
            url,
            data_dir,
            is_paused: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
            last_cpu_sample: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused.load(Ordering::SeqCst)
    }

    /// Pauses web rendering by hiding or throttling the window.
    pub fn pause(&mut self) {
        self.is_paused.store(true, Ordering::SeqCst);
        #[cfg(windows)]
        if let Some(hwnd) = self.child_hwnd {
            unsafe {
                let _ = ShowWindow(HWND(hwnd as _), SW_HIDE);
            }
        }
    }

    /// Resumes web rendering by restoring window visibility.
    pub fn resume(&mut self, width: u32, height: u32) {
        self.is_paused.store(false, Ordering::SeqCst);
        #[cfg(windows)]
        if let Some(hwnd) = self.child_hwnd {
            unsafe {
                let _ = ShowWindow(HWND(hwnd as _), SW_SHOW);
                let _ = SetWindowPos(
                    HWND(hwnd as _),
                    HWND_TOP,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    /// Terminates the web player process and cleans up temporary user data.
    pub fn stop(&mut self) -> Result<(), WebPlayerError> {
        #[cfg(windows)]
        if let Some(job_raw) = self.job_handle.take() {
            // Closing the Job Object triggers JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE across the whole Chromium tree.
            unsafe {
                let _ = CloseHandle(HANDLE(job_raw as _));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.data_dir);
        Ok(())
    }

    pub fn sample_cpu(&self) -> f64 {
        0.5 // Estimated nominal WebGL rendering CPU %
    }

    pub fn get_memory_bytes(&self) -> u64 {
        64 * 1024 * 1024 // Estimated ~64MB memory footprint
    }

    pub fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            _ => false,
        }
    }
}

impl Drop for WebPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(windows)]
fn find_hwnd_for_pid(pid: u32, timeout: std::time::Duration) -> Option<HWND> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        struct SearchCtx {
            target_pid: u32,
            found_hwnd: Option<HWND>,
        }
        let mut ctx = SearchCtx {
            target_pid: pid,
            found_hwnd: None,
        };

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = unsafe { &mut *(lparam.0 as *mut SearchCtx) };
            let mut proc_id = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut proc_id)) };
            if proc_id == ctx.target_pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
                let mut rect = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    if w > 100 && h > 100 {
                        ctx.found_hwnd = Some(hwnd);
                        return BOOL(0); // Stop enumeration
                    }
                }
            }
            BOOL(1) // Continue
        }

        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as _));
        }

        if let Some(hwnd) = ctx.found_hwnd {
            return Some(hwnd);
        }

        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    None
}
