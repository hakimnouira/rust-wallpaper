//! MPV process supervisor, native HWND embedding, and Named Pipe IPC client.
//!
//! # Architecture & Process Isolation
//!
//! Rather than loading decoders inside the coordinator daemon, video playback runs in an
//! isolated `mpv.exe` process. The video surface is embedded directly into the native Win32
//! wallpaper window using `--wid=<HWND>`, ensuring seamless desktop integration underneath
//! desktop icons without creating an extra top-level window.
//!
//! # Process Supervision & Safety Guarantee
//!
//! On Windows, each spawned MPV instance is assigned to a Windows Job Object configured with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. This guarantees at the Windows kernel level that if the
//! daemon crashes or exits, all MPV player processes are immediately and unconditionally
//! terminated by the operating system, preventing orphaned background processes.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;
use wallpaper_core::{PlaybackConfig, PlaybackState};

type MpvResponse = Result<Value, MpvError>;
type PendingResponses = Arc<Mutex<HashMap<u64, Sender<MpvResponse>>>>;
type ProbeResults = Arc<Mutex<VecDeque<(bool, bool)>>>;

#[derive(Debug, Error)]
pub enum MpvError {
    #[error("MPV executable not found on system")]
    ExecutableNotFound,
    #[error("failed to spawn MPV process: {0}")]
    SpawnFailed(std::io::Error),
    #[error("failed to connect to MPV IPC pipe: {0}")]
    IpcConnectionFailed(String),
    #[error("IPC communication error: {0}")]
    IpcIoError(#[from] std::io::Error),
    #[error("JSON serialization/deserialization error: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("Windows API error: {0}")]
    WindowsApi(String),
    #[error("player error: {0}")]
    Player(String),
}

/// Discovers the path to `mpv.exe` on Windows.
pub fn find_mpv_executable() -> Result<PathBuf, MpvError> {
    // 1. Check relative to current running executable
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            let candidates = [
                exe_dir.join("mpv.exe"),
                exe_dir.join("bin").join("mpv.exe"),
                exe_dir.join("bin").join("mpv").join("mpv.exe"),
            ];
            for candidate in candidates {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    // 2. Check local working directory bin/mpv
    let local_paths = [
        PathBuf::from("bin").join("mpv").join("mpv.exe"),
        PathBuf::from("bin").join("mpv.exe"),
        PathBuf::from("mpv.exe"),
    ];
    for local_path in local_paths {
        if local_path.is_file() {
            return Ok(local_path);
        }
    }

    // 3. Check user profile .mpv
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let profile_path = PathBuf::from(user_profile).join(".mpv").join("mpv.exe");
        if profile_path.is_file() {
            return Ok(profile_path);
        }
    }

    // 4. Check system PATH
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("mpv.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(MpvError::ExecutableNotFound)
}

/// Configuration settings for an MPV player instance.
#[derive(Debug, Clone)]
pub struct MpvLaunchConfig {
    pub video_path: PathBuf,
    pub wid: isize,
    pub config: PlaybackConfig,
}

#[cfg(windows)]
pub struct MpvProcess {
    process_handle: windows::Win32::Foundation::HANDLE,
    thread_handle: windows::Win32::Foundation::HANDLE,
    job_handle: windows::Win32::Foundation::HANDLE,
    pid: u32,
}

#[cfg(windows)]
impl MpvProcess {
    pub fn spawn(cmd_line: &str) -> Result<Self, MpvError> {
        use windows::core::PWSTR;
        use windows::Win32::Foundation::{CloseHandle, GetLastError};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows::Win32::System::Threading::{
            CreateProcessW, ResumeThread, TerminateProcess, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        // 1. Create Job Object with KILL_ON_JOB_CLOSE so child never orphans
        let job = unsafe {
            let job = CreateJobObjectW(None, None)
                .map_err(|e| MpvError::WindowsApi(format!("CreateJobObject failed: {e}")))?;

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            if let Err(e) = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) {
                let _ = CloseHandle(job);
                return Err(MpvError::WindowsApi(format!(
                    "SetInformationJobObject failed: {e}"
                )));
            }
            job
        };

        // Determine current thread's desktop name so MPV lands on the exact same desktop
        let mut desk_name = "default\0".to_string();
        unsafe {
            use windows::Win32::System::StationsAndDesktops::{
                GetThreadDesktop, GetUserObjectInformationW, UOI_NAME,
            };
            use windows::Win32::System::Threading::GetCurrentThreadId;

            let hdesk = GetThreadDesktop(GetCurrentThreadId());
            if let Ok(hdesk) = hdesk {
                let mut buf = [0u16; 256];
                let mut len = 0u32;
                let handle = windows::Win32::Foundation::HANDLE(hdesk.0);
                if GetUserObjectInformationW(
                    handle,
                    UOI_NAME,
                    Some(buf.as_mut_ptr() as *mut _),
                    (buf.len() * 2) as u32,
                    Some(&mut len),
                )
                .is_ok()
                {
                    let name =
                        String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
                    if !name.is_empty() {
                        desk_name = format!("{}\0", name);
                    }
                }
            }
        }

        let mut desktop_wide: Vec<u16> = desk_name.encode_utf16().collect();
        let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().chain(std::iter::once(0)).collect();

        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop_wide.as_mut_ptr()),
            dwFlags: STARTF_USESHOWWINDOW,
            wShowWindow: SW_HIDE.0 as u16,
            ..Default::default()
        };

        let mut pi = PROCESS_INFORMATION::default();

        // 2. Create process in SUSPENDED state with CREATE_NO_WINDOW
        let create_res = unsafe {
            CreateProcessW(
                None,
                PWSTR(cmd_wide.as_mut_ptr()),
                None,
                None,
                false,
                CREATE_SUSPENDED | CREATE_NO_WINDOW,
                None,
                None,
                &si,
                &mut pi,
            )
        };

        if let Err(e) = create_res {
            unsafe {
                let _ = CloseHandle(job);
            }
            return Err(MpvError::SpawnFailed(std::io::Error::other(e.to_string())));
        }

        // 3. Assign to Job Object before resuming execution
        let assign_res = unsafe { AssignProcessToJobObject(job, pi.hProcess) };
        if let Err(e) = assign_res {
            unsafe {
                let _ = TerminateProcess(pi.hProcess, 1);
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(job);
            }
            return Err(MpvError::WindowsApi(format!(
                "AssignProcessToJobObject failed: {e}"
            )));
        }

        // 4. Resume primary thread
        let resume_res = unsafe { ResumeThread(pi.hThread) };
        if resume_res == 0xFFFFFFFF {
            let err = unsafe { GetLastError() };
            unsafe {
                let _ = TerminateProcess(pi.hProcess, 1);
                let _ = CloseHandle(pi.hThread);
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(job);
            }
            return Err(MpvError::WindowsApi(format!(
                "ResumeThread failed: {err:?}"
            )));
        }

        Ok(Self {
            process_handle: pi.hProcess,
            thread_handle: pi.hThread,
            job_handle: job,
            pid: pi.dwProcessId,
        })
    }

    pub fn id(&self) -> u32 {
        self.pid
    }

    pub fn try_wait(&self) -> Result<Option<u32>, std::io::Error> {
        use windows::Win32::Foundation::{GetLastError, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        unsafe {
            let wait = WaitForSingleObject(self.process_handle, 0);
            match wait {
                WAIT_OBJECT_0 => {
                    let mut code = 0u32;
                    GetExitCodeProcess(self.process_handle, &mut code).map_err(|e| {
                        std::io::Error::other(format!("GetExitCodeProcess failed: {e}"))
                    })?;
                    Ok(Some(code))
                }
                WAIT_TIMEOUT => Ok(None),
                WAIT_FAILED => {
                    let err = GetLastError();
                    Err(std::io::Error::other(format!(
                        "WaitForSingleObject WAIT_FAILED: {err:?}"
                    )))
                }
                other => Err(std::io::Error::other(format!(
                    "WaitForSingleObject unexpected result: {other:?}"
                ))),
            }
        }
    }

    pub fn kill(&mut self) -> Result<(), std::io::Error> {
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

        unsafe {
            if !self.process_handle.is_invalid() && !self.process_handle.0.is_null() {
                if WaitForSingleObject(self.process_handle, 0) == WAIT_OBJECT_0 {
                    return Ok(());
                }
                if let Err(e) = TerminateProcess(self.process_handle, 1) {
                    // Windows returns ERROR_ACCESS_DENIED if the process has already exited or is in teardown.
                    // Allow up to 1 second for the process handle to become signaled.
                    if WaitForSingleObject(self.process_handle, 1000) != WAIT_OBJECT_0 {
                        return Err(std::io::Error::other(format!(
                            "TerminateProcess failed: {e}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn wait(&mut self) -> Result<u32, std::io::Error> {
        use windows::Win32::Foundation::{GetLastError, WAIT_FAILED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        unsafe {
            let wait = WaitForSingleObject(self.process_handle, 0xFFFFFFFF);
            match wait {
                WAIT_OBJECT_0 => {
                    let mut code = 0u32;
                    GetExitCodeProcess(self.process_handle, &mut code).map_err(|e| {
                        std::io::Error::other(format!("GetExitCodeProcess failed: {e}"))
                    })?;
                    Ok(code)
                }
                WAIT_FAILED => {
                    let err = GetLastError();
                    Err(std::io::Error::other(format!(
                        "WaitForSingleObject WAIT_FAILED: {err:?}"
                    )))
                }
                other => Err(std::io::Error::other(format!(
                    "WaitForSingleObject unexpected result: {other:?}"
                ))),
            }
        }
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<u32>, std::io::Error> {
        use windows::Win32::Foundation::{GetLastError, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        unsafe {
            let wait = WaitForSingleObject(self.process_handle, millis);
            match wait {
                WAIT_OBJECT_0 => {
                    let mut code = 0u32;
                    GetExitCodeProcess(self.process_handle, &mut code).map_err(|e| {
                        std::io::Error::other(format!("GetExitCodeProcess failed: {e}"))
                    })?;
                    Ok(Some(code))
                }
                WAIT_TIMEOUT => Ok(None),
                WAIT_FAILED => {
                    let err = GetLastError();
                    Err(std::io::Error::other(format!(
                        "WaitForSingleObject WAIT_FAILED: {err:?}"
                    )))
                }
                other => Err(std::io::Error::other(format!(
                    "WaitForSingleObject unexpected result: {other:?}"
                ))),
            }
        }
    }

    pub fn raw_handle(&self) -> windows::Win32::Foundation::HANDLE {
        self.process_handle
    }
}

#[cfg(windows)]
impl Drop for MpvProcess {
    fn drop(&mut self) {
        unsafe {
            if !self.job_handle.is_invalid() && !self.job_handle.0.is_null() {
                let _ = windows::Win32::Foundation::CloseHandle(self.job_handle);
                self.job_handle = windows::Win32::Foundation::HANDLE::default();
            }
            if !self.thread_handle.is_invalid() && !self.thread_handle.0.is_null() {
                let _ = windows::Win32::Foundation::CloseHandle(self.thread_handle);
                self.thread_handle = windows::Win32::Foundation::HANDLE::default();
            }
            if !self.process_handle.is_invalid() && !self.process_handle.0.is_null() {
                let _ = windows::Win32::Foundation::CloseHandle(self.process_handle);
                self.process_handle = windows::Win32::Foundation::HANDLE::default();
            }
        }
    }
}

#[cfg(not(windows))]
pub struct MpvProcess(Child);

#[cfg(not(windows))]
impl MpvProcess {
    pub fn id(&self) -> u32 {
        self.0.id()
    }
    pub fn try_wait(&mut self) -> Result<Option<u32>, std::io::Error> {
        self.0
            .try_wait()
            .map(|opt| opt.map(|s| s.code().unwrap_or(0) as u32))
    }
    pub fn kill(&mut self) -> Result<(), std::io::Error> {
        self.0.kill()
    }
    pub fn wait(&mut self) -> Result<u32, std::io::Error> {
        self.0.wait().map(|s| s.code().unwrap_or(0) as u32)
    }
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<u32>, std::io::Error> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.0.try_wait()? {
                return Ok(Some(status.code().unwrap_or(0) as u32));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(None)
    }
}

/// Supervised MPV player instance controlling an embedded video stream.
pub struct MpvPlayer {
    child: MpvProcess,
    pipe_path: String,
    pipe_writer: Arc<Mutex<Option<File>>>,
    pending_responses: PendingResponses,
    reader_running: Arc<AtomicBool>,
    active_decoder: String,
    state: Arc<Mutex<PlaybackState>>,
    command_tx: SyncSender<MpvCommand>,
    probe_in_flight: Arc<AtomicBool>,
    probe_results: ProbeResults,
    state_command_in_flight: Arc<AtomicBool>,
    volume: u32,
    muted: bool,
    started_at: Instant,
    #[cfg(windows)]
    last_cpu_time: Arc<AtomicU64>,
    #[cfg(windows)]
    last_cpu_sample_instant: Arc<Mutex<Instant>>,
    log_path: Option<PathBuf>,
    #[doc(hidden)]
    mock_stop_failure: bool,
}

struct MpvCommand {
    command: Vec<Value>,
    state_on_success: Option<PlaybackState>,
    probe_result: Option<(Arc<AtomicBool>, ProbeResults)>,
    state_command_guard: Option<Arc<AtomicBool>>,
    reply: Option<Sender<Result<Option<Value>, MpvError>>>,
}

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);
const COMMAND_QUEUE_CAPACITY: usize = 64;
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);

impl MpvPlayer {
    /// Launches an MPV instance embedded in `wid` and establishes IPC control.
    pub fn launch(launch_cfg: MpvLaunchConfig) -> Result<Self, MpvError> {
        if !launch_cfg.video_path.exists() {
            return Err(MpvError::Player(format!(
                "Video file not found at path: {}",
                launch_cfg.video_path.display()
            )));
        }

        let mpv_bin = find_mpv_executable()?;
        let pipe_id = Uuid::new_v4();
        let pipe_name = format!("rust-wallpaper-mpv-{}", pipe_id);
        let pipe_path = format!(r"\\.\pipe\{}", pipe_name);

        info!(
            mpv = %mpv_bin.display(),
            video = %launch_cfg.video_path.display(),
            wid = format_args!("0x{:X}", launch_cfg.wid),
            hwdec = ?launch_cfg.config.hardware_acceleration,
            "Launching embedded MPV player"
        );

        let hwdec_arg = launch_cfg.config.hardware_acceleration.as_mpv_arg();
        let loop_arg = if launch_cfg.config.loop_playback {
            "inf"
        } else {
            "no"
        };

        crate::desktop_host::ensure_interactive_desktop();

        let log_path = std::env::temp_dir().join(format!("mpv-{}.log", pipe_id));
        let trace_mpv = std::env::var("RUST_WALLPAPER_MPV_TRACE")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        let msg_level_arg = if trace_mpv {
            "--msg-level=all=trace".to_string()
        } else {
            "--msg-level=all=warn".to_string()
        };

        let mut cmd_args = vec![
            format!("\"{}\"", mpv_bin.display()),
            format!("--wid={}", launch_cfg.wid as usize),
            format!("--input-ipc-server={}", pipe_path),
            format!("--log-file={}", log_path.display()),
            msg_level_arg,
            format!("--hwdec={}", hwdec_arg),
            "--hwdec-codecs=all".to_string(),
            "--hwdec-software-fallback=yes".to_string(),
            "--audio-display=no".to_string(),
            format!("--loop-file={}", loop_arg),
            "--image-display-duration=inf".to_string(),
            format!("--volume={}", launch_cfg.config.volume),
            "--idle=yes".to_string(),
            "--force-window=yes".to_string(),
            "--vo=gpu".to_string(),
            "--gpu-context=d3d11".to_string(),
            "--no-border".to_string(),
            "--no-osc".to_string(),
            "--no-osd-bar".to_string(),
            "--input-default-bindings=no".to_string(),
            "--input-vo-keyboard=no".to_string(),
            "--cursor-autohide=no".to_string(),
        ];

        if launch_cfg.config.mute {
            cmd_args.push("--mute=yes".to_string());
        }

        // Video rotation (0, 90, 180, 270 degrees)
        if let Some(rotation) = launch_cfg.config.video_rotation {
            if rotation > 0 {
                cmd_args.push(format!("--video-rotate={rotation}"));
            }
        }

        // Framerate limit: avoid CPU-copying `--vf=fps` when hardware decoding is enabled
        if let Some(target_fps) = launch_cfg.config.target_fps {
            if target_fps > 0 {
                if launch_cfg.config.hardware_acceleration
                    == wallpaper_core::HardwareAcceleration::Disabled
                {
                    cmd_args.push(format!("--vf=fps=fps={target_fps}"));
                } else {
                    cmd_args.push(format!("--override-display-fps={target_fps}"));
                }
            }
        }

        // Memory budget / RAM limit (Wallpaper Engine style)
        if let Some(ram_mb) = launch_cfg.config.ram_budget_mb {
            if ram_mb > 0 {
                cmd_args.push(format!("--demuxer-max-bytes={ram_mb}M"));
                cmd_args.push(format!("--demuxer-max-back-bytes={}M", (ram_mb / 2).max(4)));
            }
        }

        // Scaling quality preset
        match launch_cfg.config.quality {
            wallpaper_core::VideoQuality::Fast => {
                cmd_args.push("--scale=bilinear".to_string());
                cmd_args.push("--cscale=bilinear".to_string());
                cmd_args.push("--dscale=bilinear".to_string());
            }
            wallpaper_core::VideoQuality::Balanced => {
                cmd_args.push("--scale=spline36".to_string());
                cmd_args.push("--cscale=spline36".to_string());
            }
            wallpaper_core::VideoQuality::High => {
                cmd_args.push("--scale=ewa_lanczos".to_string());
                cmd_args.push("--cscale=ewa_lanczos".to_string());
            }
        }

        cmd_args.push("--".to_string());
        cmd_args.push(format!("\"{}\"", launch_cfg.video_path.display()));

        let cmd_line = cmd_args.join(" ");

        #[cfg(windows)]
        let child = MpvProcess::spawn(&cmd_line)?;

        #[cfg(not(windows))]
        let mut child = {
            let mut cmd = Command::new(&mpv_bin);
            for arg in &cmd_args[1..] {
                cmd.arg(arg);
            }
            MpvProcess(cmd.spawn().map_err(MpvError::SpawnFailed)?)
        };

        // Connect to Named Pipe with retry timeout
        let connect_deadline = Instant::now() + Duration::from_millis(2500);
        let mut pipe_file = None;

        while Instant::now() < connect_deadline {
            if let Ok(Some(status)) = child.try_wait() {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(MpvError::Player(format!(
                    "MPV child process exited unexpectedly with status {status:?}"
                )));
            }

            #[cfg(windows)]
            {
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
                };
                let mut msg = MSG::default();
                unsafe {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }

            match OpenOptions::new().read(true).write(true).open(&pipe_path) {
                Ok(file) => {
                    pipe_file = Some(file);
                    break;
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }

        // If pipe cannot be connected, clean up child and return error
        let file = match pipe_file {
            Some(f) => f,
            None => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(MpvError::IpcConnectionFailed(format!(
                    "Timed out waiting to connect to MPV IPC pipe '{pipe_path}'"
                )));
            }
        };

        // If cloning pipe handle fails, clean up child and return error
        let writer_clone = match file.try_clone() {
            Ok(w) => w,
            Err(e) => {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(MpvError::IpcIoError(e));
            }
        };

        let pending_responses: PendingResponses = Arc::new(Mutex::new(HashMap::new()));
        let reader_running = Arc::new(AtomicBool::new(true));
        let pipe_writer = Arc::new(Mutex::new(Some(writer_clone)));

        #[cfg(windows)]
        use std::os::windows::io::AsRawHandle;
        #[cfg(windows)]
        let raw_handle = file.as_raw_handle() as isize;

        let mut reader_file = file;
        let pending_clone = Arc::clone(&pending_responses);
        let running_clone = Arc::clone(&reader_running);

        // Dedicated background reader thread with non-blocking peek and stream accumulator
        // Handles fragmented/partial JSON lines safely without blocking in ReadFile on shutdown.
        let reader_spawn = std::thread::Builder::new()
            .name("mpv-ipc-reader".into())
            .spawn(move || {
                #[cfg(windows)]
                extern "system" {
                    fn PeekNamedPipe(
                        hNamedPipe: windows::Win32::Foundation::HANDLE,
                        lpBuffer: *mut core::ffi::c_void,
                        nBufferSize: u32,
                        lpBytesRead: *mut u32,
                        lpTotalBytesAvail: *mut u32,
                        lpBytesLeftThisMessage: *mut u32,
                    ) -> windows::Win32::Foundation::BOOL;
                }
                #[cfg(windows)]
                let pipe_handle =
                    windows::Win32::Foundation::HANDLE(raw_handle as *mut core::ffi::c_void);

                let mut accumulator = String::with_capacity(4096);
                let mut buf = [0u8; 4096];

                while running_clone.load(Ordering::SeqCst) {
                    #[cfg(windows)]
                    {
                        let mut bytes_avail = 0u32;
                        let ok = unsafe {
                            PeekNamedPipe(
                                pipe_handle,
                                std::ptr::null_mut(),
                                0,
                                std::ptr::null_mut(),
                                &mut bytes_avail,
                                std::ptr::null_mut(),
                            )
                            .as_bool()
                        };
                        if ok {
                            if bytes_avail == 0 {
                                std::thread::sleep(Duration::from_millis(5));
                                continue;
                            }
                        } else {
                            break; // Pipe broken or closed by MPV
                        }

                        let to_read = (bytes_avail as usize).min(buf.len());
                        match reader_file.read(&mut buf[..to_read]) {
                            Ok(0) => break, // EOF
                            Ok(n) => {
                                accumulator.push_str(&String::from_utf8_lossy(&buf[..n]));
                                while let Some(pos) = accumulator.find('\n') {
                                    let line: String = accumulator[..pos].trim().to_string();
                                    accumulator.drain(..=pos);
                                    if line.is_empty() {
                                        continue;
                                    }
                                    if let Ok(parsed) = serde_json::from_str::<Value>(&line) {
                                        if let Some(req_id) =
                                            parsed.get("request_id").and_then(|v| v.as_u64())
                                        {
                                            let mut map = pending_clone.lock().unwrap();
                                            if let Some(tx) = map.remove(&req_id) {
                                                let _ = tx.send(Ok(parsed));
                                            }
                                        }
                                        // Asynchronous events (without request_id) pass through harmlessly
                                    }
                                }
                            }
                            Err(_) => break, // Pipe broken
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                running_clone.store(false, Ordering::SeqCst);

                // Drain and complete all pending requests with error upon pipe disconnect
                let mut map = pending_clone.lock().unwrap();
                for (_id, tx) in map.drain() {
                    let _ = tx.send(Err(MpvError::IpcConnectionFailed(
                        "IPC pipe disconnected while request was pending".into(),
                    )));
                }
            });

        // If reader thread fails to start, clean up child and return error
        if let Err(e) = reader_spawn {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(MpvError::Player(format!(
                "Failed to spawn reader thread: {e}"
            )));
        }

        let state = Arc::new(Mutex::new(PlaybackState::Starting));
        let probe_in_flight = Arc::new(AtomicBool::new(false));
        let probe_results = Arc::new(Mutex::new(VecDeque::new()));
        let state_command_in_flight = Arc::new(AtomicBool::new(false));
        let (command_tx, command_rx) = sync_channel(COMMAND_QUEUE_CAPACITY);
        let command_spawn = std::thread::Builder::new()
            .name("mpv-ipc-command".into())
            .spawn({
                let writer = Arc::clone(&pipe_writer);
                let pending = Arc::clone(&pending_responses);
                let worker_state = Arc::clone(&state);
                move || Self::command_loop(command_rx, writer, pending, worker_state)
            });
        if let Err(error) = command_spawn {
            reader_running.store(false, Ordering::SeqCst);
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(MpvError::Player(format!(
                "Failed to spawn MPV command worker: {error}"
            )));
        }

        let mut player = Self {
            child,
            pipe_path,
            pipe_writer,
            pending_responses,
            reader_running,
            active_decoder: "unknown".to_string(),
            state,
            command_tx,
            probe_in_flight,
            probe_results,
            state_command_in_flight,
            volume: launch_cfg.config.volume,
            muted: launch_cfg.config.mute,
            started_at: Instant::now(),
            #[cfg(windows)]
            last_cpu_time: Arc::new(AtomicU64::new(0)),
            #[cfg(windows)]
            last_cpu_sample_instant: Arc::new(Mutex::new(Instant::now())),
            log_path: if trace_mpv { None } else { Some(log_path) },
            mock_stop_failure: false,
        };

        // Query active decoder through IPC with bounded polling
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut detected = "software".to_string();
        while Instant::now() < deadline {
            #[cfg(windows)]
            {
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
                };
                let mut msg = MSG::default();
                unsafe {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }

            if let Ok(decoder) = player.query_active_decoder() {
                if !decoder.is_empty() && decoder != "no" {
                    detected = decoder;
                    if detected != "software"
                        || launch_cfg.config.hardware_acceleration
                            == wallpaper_core::HardwareAcceleration::Disabled
                    {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        info!(
            active_decoder = %detected,
            requested = ?launch_cfg.config.hardware_acceleration,
            "Hardware decoding telemetry verified"
        );
        player.active_decoder = detected;
        *player.state.lock().unwrap() = PlaybackState::Playing;

        Ok(player)
    }

    pub fn pipe_path(&self) -> &str {
        &self.pipe_path
    }

    /// Serializes player commands through a dedicated worker so independent monitors do not
    /// contend on MPV's pipe writer or response map.
    fn command_loop(
        receiver: Receiver<MpvCommand>,
        writer: Arc<Mutex<Option<File>>>,
        pending: PendingResponses,
        state: Arc<Mutex<PlaybackState>>,
    ) {
        while let Ok(request) = receiver.recv() {
            let req_id = REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst);
            let probe_result = request.probe_result.clone();
            let result: Result<Option<Value>, MpvError> = (|| {
                let payload = json!({ "command": request.command, "request_id": req_id });
                let mut message = serde_json::to_vec(&payload)?;
                message.push(b'\n');
                let (tx, rx) = channel();
                pending.lock().unwrap().insert(req_id, tx);
                {
                    let mut writer = writer.lock().unwrap();
                    let Some(writer) = writer.as_mut() else {
                        pending.lock().unwrap().remove(&req_id);
                        return Err(MpvError::IpcConnectionFailed(
                            "IPC pipe writer not connected".into(),
                        ));
                    };
                    if let Err(error) = writer.write_all(&message).and_then(|_| writer.flush()) {
                        pending.lock().unwrap().remove(&req_id);
                        return Err(MpvError::IpcIoError(error));
                    }
                }
                let parsed = match rx.recv_timeout(COMMAND_TIMEOUT) {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => return Err(error),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        pending.lock().unwrap().remove(&req_id);
                        return Err(MpvError::IpcConnectionFailed(format!(
                            "Timed out waiting for response to request_id {req_id}"
                        )));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(MpvError::IpcConnectionFailed(
                            "IPC response channel disconnected".into(),
                        ));
                    }
                };
                if let Some(status) = parsed.get("error").and_then(Value::as_str) {
                    if status != "success" && status != "property unavailable" {
                        return Err(MpvError::Player(format!("MPV command failed: {status}")));
                    }
                }
                Ok(parsed.get("data").cloned())
            })();

            if let Some((in_flight, results)) = probe_result {
                let probe = match &result {
                    Ok(data) => {
                        let initialized = data.as_ref().is_some_and(|value| {
                            let width = value
                                .get("w")
                                .or_else(|| value.get("dw"))
                                .and_then(Value::as_u64);
                            let height = value
                                .get("h")
                                .or_else(|| value.get("dh"))
                                .and_then(Value::as_u64);
                            matches!((width, height), (Some(w), Some(h)) if w > 0 && h > 0)
                        });
                        (true, initialized)
                    }
                    Err(_) => (false, false),
                };
                results.lock().unwrap().push_back(probe);
                in_flight.store(false, Ordering::SeqCst);
            }
            if let Some(in_flight) = request.state_command_guard.as_ref() {
                in_flight.store(false, Ordering::SeqCst);
            }

            if let (Ok(_), Some(new_state)) = (&result, request.state_on_success) {
                *state.lock().unwrap() = new_state;
            }
            if let Some(reply) = request.reply {
                let _ = reply.send(result);
            } else if let Err(error) = result {
                warn!(%error, "Queued MPV command failed");
            }
        }
    }

    /// Queues a playback-state change and returns immediately. The reported state changes only
    /// after MPV acknowledges the command, so the runtime reconciler can retry failures.
    pub fn queue_property_command(&self, property: &str, value: Value) -> Result<(), MpvError> {
        self.command_tx
            .try_send(MpvCommand {
                command: vec![json!("set_property"), json!(property), value],
                state_on_success: None,
                probe_result: None,
                state_command_guard: None,
                reply: None,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => MpvError::Player("MPV command queue is full".into()),
                TrySendError::Disconnected(_) => {
                    MpvError::IpcConnectionFailed("MPV command worker stopped".into())
                }
            })
    }

    pub fn queue_state_command(
        &self,
        command: &[Value],
        state_on_success: PlaybackState,
    ) -> Result<(), MpvError> {
        if self.state_command_in_flight.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Err(error) = self.command_tx.try_send(MpvCommand {
            command: command.to_vec(),
            state_on_success: Some(state_on_success),
            probe_result: None,
            state_command_guard: Some(Arc::clone(&self.state_command_in_flight)),
            reply: None,
        }) {
            self.state_command_in_flight.store(false, Ordering::SeqCst);
            return Err(match error {
                TrySendError::Full(_) => MpvError::Player("MPV command queue is full".into()),
                TrySendError::Disconnected(_) => {
                    MpvError::IpcConnectionFailed("MPV command worker stopped".into())
                }
            });
        }
        Ok(())
    }

    /// Sends a JSON command to MPV and waits for a bounded response. This is intended for
    /// infrequent setup and telemetry operations; playback reconciliation uses queued commands.
    pub fn send_command(&mut self, command: &[Value]) -> Result<Option<Value>, MpvError> {
        let (reply, response) = channel();
        self.command_tx
            .try_send(MpvCommand {
                command: command.to_vec(),
                state_on_success: None,
                probe_result: None,
                state_command_guard: None,
                reply: Some(reply),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => MpvError::Player("MPV command queue is full".into()),
                TrySendError::Disconnected(_) => {
                    MpvError::IpcConnectionFailed("MPV command worker stopped".into())
                }
            })?;
        response
            .recv_timeout(COMMAND_TIMEOUT + Duration::from_millis(100))
            .map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    MpvError::IpcConnectionFailed("Timed out waiting for MPV command worker".into())
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    MpvError::IpcConnectionFailed("MPV command worker stopped".into())
                }
            })?
    }

    /// Queries the currently active hardware decoder from MPV.
    pub fn query_active_decoder(&mut self) -> Result<String, MpvError> {
        let resp = self.send_command(&[json!("get_property"), json!("hwdec-current")])?;
        match resp {
            Some(Value::String(s)) => Ok(s),
            _ => Ok("software".to_string()),
        }
    }

    /// Queries the currently active playback time position in seconds.
    pub fn query_time_pos(&mut self) -> Result<f64, MpvError> {
        let resp = self.send_command(&[json!("get_property"), json!("time-pos")])?;
        match resp {
            Some(Value::Number(num)) => num
                .as_f64()
                .ok_or_else(|| MpvError::Player("Invalid numeric value for time-pos".to_string())),
            Some(Value::Null) | None => Err(MpvError::Player(
                "time-pos property unavailable".to_string(),
            )),
            _ => Err(MpvError::Player("Unexpected type for time-pos".to_string())),
        }
    }

    /// Checks if a genuine hardware decoder is currently active (e.g. `d3d11va`, `dxva2`).
    /// Returns false if hardware acceleration fell back to software or is unavailable.
    pub fn is_hardware_decoding_active(&self) -> bool {
        !self.active_decoder.is_empty()
            && self.active_decoder != "no"
            && self.active_decoder != "software"
            && self.active_decoder != "unknown"
    }

    /// Pauses playback.
    pub fn pause(&mut self) -> Result<(), MpvError> {
        self.send_command(&[json!("set_property"), json!("pause"), json!(true)])?;
        *self.state.lock().unwrap() = PlaybackState::Paused;
        info!("Video playback paused");
        Ok(())
    }

    /// Resumes playback.
    pub fn resume(&mut self) -> Result<(), MpvError> {
        self.send_command(&[json!("set_property"), json!("pause"), json!(false)])?;
        *self.state.lock().unwrap() = PlaybackState::Playing;
        info!("Video playback resumed");
        Ok(())
    }

    /// Sets playback volume (0 to 100).
    pub fn set_volume(&mut self, volume: u32) -> Result<(), MpvError> {
        let clamped = volume.min(100);
        self.send_command(&[json!("set_property"), json!("volume"), json!(clamped)])?;
        self.volume = clamped;
        debug!(volume = clamped, "Updated playback volume");
        Ok(())
    }

    /// Toggles audio mute state.
    pub fn set_mute(&mut self, muted: bool) -> Result<(), MpvError> {
        self.send_command(&[json!("set_property"), json!("mute"), json!(muted)])?;
        self.muted = muted;
        debug!(muted, "Updated mute state");
        Ok(())
    }

    fn cleanup_log_file(&mut self) {
        if let Some(path) = self.log_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Stops playback and terminates MPV.
    pub fn stop(&mut self) -> Result<(), MpvError> {
        if self.mock_stop_failure {
            return Err(MpvError::Player(
                "Injected mock shutdown failure for testing".to_string(),
            ));
        }

        if self.state() == PlaybackState::Stopped {
            // Repeated stop calls are safe: verify process is actually dead
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.cleanup_log_file();
                    return Ok(());
                }
                Ok(None) => {
                    // Process still alive despite stopped flag, fall through to force-kill
                }
                Err(e) => {
                    return Err(MpvError::Player(format!(
                        "Failed to query child status: {e}"
                    )));
                }
            }
        }
        info!("Stopping video player process");

        // Send fire-and-forget quit command to MPV without waiting for reply
        if let Ok(mut writer_guard) = self.pipe_writer.lock() {
            if let Some(writer) = writer_guard.as_mut() {
                let quit_msg = "{\"command\":[\"quit\"]}\n";
                let _ = writer.write_all(quit_msg.as_bytes());
                let _ = writer.flush();
            }
        }
        self.reader_running.store(false, Ordering::SeqCst);
        *self.pipe_writer.lock().unwrap() = None;

        // Drain pending requests with error so waiting callers do not hang
        {
            let mut pending = self.pending_responses.lock().unwrap();
            for (_id, tx) in pending.drain() {
                let _ = tx.send(Err(MpvError::Player(
                    "Player stopped while request was pending".into(),
                )));
            }
        }

        // 1. Bounded wait for graceful exit
        let wait_deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < wait_deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    *self.state.lock().unwrap() = PlaybackState::Stopped;
                    self.cleanup_log_file();
                    return Ok(());
                }
                Ok(None) => {
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(e) => {
                    return Err(MpvError::Player(format!(
                        "Error querying child exit state: {e}"
                    )));
                }
            }
        }

        // 2. Graceful quit timed out, terminate child process
        self.child
            .kill()
            .map_err(|e| MpvError::Player(format!("Failed to kill MPV process: {e}")))?;

        // 3. Bounded wait after kill (up to 1000ms) to verify termination
        match self.child.wait_timeout(Duration::from_millis(1000)) {
            Ok(Some(_)) => {
                *self.state.lock().unwrap() = PlaybackState::Stopped;
                self.cleanup_log_file();
                Ok(())
            }
            Ok(None) => Err(MpvError::Player(
                "MPV process did not terminate within bounded kill timeout".into(),
            )),
            Err(e) => Err(MpvError::Player(format!(
                "Error waiting for MPV process exit: {e}"
            ))),
        }
    }

    /// Queries working set memory bytes of the MPV child process.
    pub fn get_memory_bytes(&self) -> u64 {
        #[cfg(windows)]
        {
            use windows::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
            };

            unsafe {
                let proc_handle = self.child.raw_handle();
                let mut counters = PROCESS_MEMORY_COUNTERS {
                    cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
                    ..Default::default()
                };

                if GetProcessMemoryInfo(proc_handle, &mut counters, counters.cb).is_ok() {
                    return counters.WorkingSetSize as u64;
                }
            }
        }
        0
    }

    /// Sets the video rotation angle (0, 90, 180, 270 degrees) dynamically on the running player.
    pub fn set_video_rotation(&mut self, rotation: u32) -> Result<(), MpvError> {
        self.send_command(&[
            serde_json::json!("set_property"),
            serde_json::json!("video-rotate"),
            serde_json::json!(rotation),
        ])?;
        Ok(())
    }

    /// Samples current CPU utilization percentage of the MPV child process.
    pub fn sample_cpu(&self) -> f64 {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::FILETIME;
            use windows::Win32::System::Threading::GetProcessTimes;

            unsafe {
                let proc_handle = self.child.raw_handle();
                let mut creation = FILETIME::default();
                let mut exit = FILETIME::default();
                let mut kernel = FILETIME::default();
                let mut user = FILETIME::default();

                if GetProcessTimes(
                    proc_handle,
                    &mut creation,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
                .is_ok()
                {
                    let cur_cpu_100ns = (((kernel.dwHighDateTime as u64) << 32)
                        | (kernel.dwLowDateTime as u64))
                        + (((user.dwHighDateTime as u64) << 32) | (user.dwLowDateTime as u64));

                    let mut last_guard = self.last_cpu_sample_instant.lock().unwrap();
                    let elapsed = last_guard.elapsed().as_secs_f64();
                    *last_guard = Instant::now();

                    let prev_cpu = self.last_cpu_time.swap(cur_cpu_100ns, Ordering::SeqCst);
                    if prev_cpu > 0 && cur_cpu_100ns >= prev_cpu && elapsed > 0.05 {
                        let delta_secs = (cur_cpu_100ns - prev_cpu) as f64 / 10_000_000.0;
                        let num_cpus = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(1) as f64;
                        let cpu_pct = (delta_secs / (elapsed * num_cpus)) * 100.0;
                        return (cpu_pct * 10.0).round() / 10.0;
                    }
                }
            }
        }
        0.0
    }

    /// Returns the PID of the MPV child process.
    pub fn child_id(&self) -> u32 {
        self.child.id()
    }

    #[doc(hidden)]
    pub fn insert_mock_pending_request(
        &self,
        id: u64,
        tx: std::sync::mpsc::Sender<Result<Value, MpvError>>,
    ) {
        let mut map = self.pending_responses.lock().unwrap();
        map.insert(id, tx);
    }

    #[doc(hidden)]
    pub fn pending_request_count(&self) -> usize {
        self.pending_responses.lock().unwrap().len()
    }

    #[doc(hidden)]
    pub fn disconnect_pipe_writer_for_test(&mut self) {
        *self.pipe_writer.lock().unwrap() = None;
    }

    /// For testing: simulate a shutdown/kill failure without relying on OS process errors.
    #[doc(hidden)]
    pub fn set_mock_stop_failure(&mut self, fail: bool) {
        self.mock_stop_failure = fail;
    }

    /// Checks if the MPV player child process is still running.
    pub fn is_alive(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                debug!(?status, "MPV child has exited");
                false
            }
            Err(e) => {
                warn!(%e, "MPV try_wait error");
                false
            }
        }
    }

    /// Returns the currently active decoder string (e.g. `d3d11va`, `dxva2`, `software`).
    pub fn active_decoder(&self) -> &str {
        &self.active_decoder
    }

    /// Returns process uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Requests an asynchronous one-round-trip health probe and returns the previous result, if
    /// one has completed. A slow or unresponsive player never blocks the daemon polling loop.
    pub fn request_video_probe(&self) -> Result<Option<(bool, bool)>, MpvError> {
        let completed = self.probe_results.lock().unwrap().pop_front();
        if !self.probe_in_flight.swap(true, Ordering::SeqCst) {
            if let Err(error) = self.command_tx.try_send(MpvCommand {
                command: vec![json!("get_property"), json!("video-params")],
                state_on_success: None,
                probe_result: Some((
                    Arc::clone(&self.probe_in_flight),
                    Arc::clone(&self.probe_results),
                )),
                state_command_guard: None,
                reply: None,
            }) {
                self.probe_in_flight.store(false, Ordering::SeqCst);
                return Err(match error {
                    TrySendError::Full(_) => MpvError::Player("MPV command queue is full".into()),
                    TrySendError::Disconnected(_) => {
                        MpvError::IpcConnectionFailed("MPV command worker stopped".into())
                    }
                });
            }
        }
        Ok(completed)
    }

    /// Synchronous compatibility helper for diagnostics and tests. Daemon health checks should
    /// use `request_video_probe` instead.
    pub fn probe_video_output(&mut self) -> Result<(bool, bool), MpvError> {
        let params = self.send_command(&[json!("get_property"), json!("video-params")])?;
        let dimensions = params.as_ref().is_some_and(|value| {
            let width = value
                .get("w")
                .or_else(|| value.get("dw"))
                .and_then(Value::as_u64);
            let height = value
                .get("h")
                .or_else(|| value.get("dh"))
                .and_then(Value::as_u64);
            matches!((width, height), (Some(w), Some(h)) if w > 0 && h > 0)
        });
        Ok((true, dimensions))
    }

    /// Queries video rendering details (width, height, fps, gpu context, estimated-vf-fps).
    pub fn query_video_details(
        &mut self,
    ) -> (Option<u32>, Option<u32>, Option<f64>, Option<String>) {
        let w = self
            .send_command(&[json!("get_property"), json!("width")])
            .ok()
            .flatten()
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let h = self
            .send_command(&[json!("get_property"), json!("height")])
            .ok()
            .flatten()
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        let fps = self
            .send_command(&[json!("get_property"), json!("estimated-vf-fps")])
            .ok()
            .flatten()
            .and_then(|v| v.as_f64())
            .or_else(|| {
                self.send_command(&[json!("get_property"), json!("container-fps")])
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_f64())
            });

        let gpu = self
            .send_command(&[json!("get_property"), json!("current-gpu-context")])
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(|s| s.to_string()));

        (w, h, fps, gpu)
    }

    /// Returns the current playback state.
    pub fn state(&self) -> PlaybackState {
        *self.state.lock().unwrap()
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
