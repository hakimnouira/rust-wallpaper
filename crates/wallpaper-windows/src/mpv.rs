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
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
#[cfg(not(windows))]
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;
use wallpaper_core::{PlaybackConfig, PlaybackState};

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
    // 1. Check local bin/mpv
    let local_path = PathBuf::from("bin").join("mpv").join("mpv.exe");
    if local_path.is_file() {
        return Ok(local_path);
    }

    // 2. Check user profile .mpv
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let profile_path = PathBuf::from(user_profile).join(".mpv").join("mpv.exe");
        if profile_path.is_file() {
            return Ok(profile_path);
        }
    }

    // 3. Check system PATH
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
            CreateProcessW, ResumeThread, TerminateProcess, CREATE_SUSPENDED, PROCESS_INFORMATION,
            STARTF_USESHOWWINDOW, STARTUPINFOW,
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
                let handle = windows::Win32::Foundation::HANDLE(hdesk.0 as *mut core::ffi::c_void);
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

        let mut si = STARTUPINFOW::default();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = PWSTR(desktop_wide.as_mut_ptr());
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE.0 as u16;

        let mut pi = PROCESS_INFORMATION::default();

        // 2. Create process in SUSPENDED state
        let create_res = unsafe {
            CreateProcessW(
                None,
                PWSTR(cmd_wide.as_mut_ptr()),
                None,
                None,
                false,
                CREATE_SUSPENDED,
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
                    if WaitForSingleObject(self.process_handle, 0) != WAIT_OBJECT_0 {
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
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, std::io::Error> {
        self.0.try_wait()
    }
    pub fn kill(&mut self) -> Result<(), std::io::Error> {
        self.0.kill()
    }
    pub fn wait(&mut self) -> Result<std::process::ExitStatus, std::io::Error> {
        self.0.wait()
    }
}

/// Supervised MPV player instance controlling an embedded video stream.
pub struct MpvPlayer {
    child: MpvProcess,
    pipe_path: String,
    pipe_writer: Arc<Mutex<Option<File>>>,
    pending_responses: Arc<Mutex<HashMap<u64, Sender<Result<Value, MpvError>>>>>,
    reader_running: Arc<AtomicBool>,
    active_decoder: String,
    state: PlaybackState,
    volume: u32,
    muted: bool,
    started_at: Instant,
}

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

impl MpvPlayer {
    /// Launches an MPV instance embedded in `wid` and establishes IPC control.
    pub fn launch(launch_cfg: MpvLaunchConfig) -> Result<Self, MpvError> {
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

        let mut cmd_args = vec![
            format!("\"{}\"", mpv_bin.display()),
            format!("--wid={}", launch_cfg.wid as usize),
            format!("--input-ipc-server={}", pipe_path),
            format!("--log-file={}", log_path.display()),
            "--msg-level=all=trace".to_string(),
            format!("--hwdec={}", hwdec_arg),
            "--hwdec-codecs=all".to_string(),
            "--hwdec-software-fallback=yes".to_string(),
            format!("--loop-file={}", loop_arg),
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

        let pending_responses: Arc<Mutex<HashMap<u64, Sender<Result<Value, MpvError>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
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

        let mut player = Self {
            child,
            pipe_path,
            pipe_writer,
            pending_responses,
            reader_running,
            active_decoder: "unknown".to_string(),
            state: PlaybackState::Starting,
            volume: launch_cfg.config.volume,
            muted: launch_cfg.config.mute,
            started_at: Instant::now(),
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
        player.state = PlaybackState::Playing;

        Ok(player)
    }

    pub fn pipe_path(&self) -> &str {
        &self.pipe_path
    }

    /// Sends a JSON command to the MPV IPC pipe.
    pub fn send_command(&mut self, command: &[Value]) -> Result<Option<Value>, MpvError> {
        let req_id = REQUEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel();

        {
            let mut pending = self.pending_responses.lock().unwrap();
            pending.insert(req_id, tx);
        }

        let payload = json!({
            "command": command,
            "request_id": req_id
        });

        let mut msg = serde_json::to_string(&payload)?;
        msg.push('\n');

        {
            let mut writer_guard = self.pipe_writer.lock().unwrap();
            if let Some(writer) = writer_guard.as_mut() {
                writer.write_all(msg.as_bytes())?;
                writer.flush()?;
            } else {
                return Err(MpvError::IpcConnectionFailed(
                    "IPC pipe writer not connected".into(),
                ));
            }
        }

        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut parsed_result = None;

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

            match rx.try_recv() {
                Ok(Ok(parsed)) => {
                    parsed_result = Some(parsed);
                    break;
                }
                Ok(Err(e)) => return Err(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(MpvError::IpcConnectionFailed(
                        "IPC response channel disconnected".into(),
                    ));
                }
            }
        }

        match parsed_result {
            Some(parsed) => {
                if let Some(err) = parsed.get("error").and_then(|e| e.as_str()) {
                    if err != "success" && err != "property unavailable" {
                        debug!(
                            request_id = req_id,
                            error = err,
                            "MPV command returned status"
                        );
                    }
                }
                Ok(parsed.get("data").cloned())
            }
            None => {
                let mut pending = self.pending_responses.lock().unwrap();
                pending.remove(&req_id);
                Err(MpvError::IpcConnectionFailed(format!(
                    "Timed out waiting for response to request_id {req_id}"
                )))
            }
        }
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
        self.state = PlaybackState::Paused;
        info!("Video playback paused");
        Ok(())
    }

    /// Resumes playback.
    pub fn resume(&mut self) -> Result<(), MpvError> {
        self.send_command(&[json!("set_property"), json!("pause"), json!(false)])?;
        self.state = PlaybackState::Playing;
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

    /// Stops playback and terminates MPV.
    pub fn stop(&mut self) -> Result<(), MpvError> {
        if self.state == PlaybackState::Stopped {
            return Ok(());
        }
        info!("Stopping video player process");
        self.state = PlaybackState::Stopped;

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

        let wait_deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < wait_deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
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
                let mut counters = PROCESS_MEMORY_COUNTERS::default();
                counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;

                if GetProcessMemoryInfo(proc_handle, &mut counters, counters.cb).is_ok() {
                    return counters.WorkingSetSize as u64;
                }
            }
        }
        0
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
        self.state
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
