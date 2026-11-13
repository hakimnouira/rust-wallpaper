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
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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

/// Supervised MPV player instance controlling an embedded video stream.
pub struct MpvPlayer {
    child: Child,
    pipe_path: String,
    pipe_writer: Arc<Mutex<Option<File>>>,
    pending_responses: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    reader_running: Arc<AtomicBool>,
    active_decoder: String,
    state: PlaybackState,
    volume: u32,
    muted: bool,
    started_at: Instant,
    #[cfg(windows)]
    job_handle: Option<windows::Win32::Foundation::HANDLE>,
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

        let mut cmd = Command::new(&mpv_bin);
        let log_path = std::env::temp_dir().join(format!("mpv-{}.log", pipe_id));
        cmd.arg(format!("--wid={}", launch_cfg.wid))
            .arg(format!("--input-ipc-server={}", pipe_path))
            .arg(format!("--log-file={}", log_path.display()))
            .arg("--msg-level=all=trace")
            .arg(format!("--hwdec={}", hwdec_arg))
            .arg("--hwdec-codecs=all")
            .arg("--hwdec-software-fallback=yes")
            .arg(format!("--loop-file={}", loop_arg))
            .arg(format!("--volume={}", launch_cfg.config.volume))
            .arg("--idle=yes")
            .arg("--force-window=yes")
            .arg("--no-border")
            .arg("--no-osc")
            .arg("--no-osd-bar")
            .arg("--input-default-bindings=no")
            .arg("--input-vo-keyboard=no")
            .arg("--cursor-autohide=no");

        if launch_cfg.config.mute {
            cmd.arg("--mute=yes");
        }

        cmd.arg("--").arg(&launch_cfg.video_path);

        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().map_err(MpvError::SpawnFailed)?;

        #[cfg(windows)]
        let job_handle = Self::attach_job_object(&child)?;

        // Connect to Named Pipe with retry timeout
        let connect_deadline = Instant::now() + Duration::from_millis(2500);
        let mut pipe_file = None;

        while Instant::now() < connect_deadline {
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

        let pending_responses = Arc::new(Mutex::new(HashMap::<u64, Sender<Value>>::new()));
        let reader_running = Arc::new(AtomicBool::new(false));
        let pipe_writer = Arc::new(Mutex::new(None));

        if let Some(file) = pipe_file {
            if let Ok(writer_clone) = file.try_clone() {
                *pipe_writer.lock().unwrap() = Some(writer_clone);
                let reader = BufReader::new(file);
                let pending_clone = Arc::clone(&pending_responses);
                let running_clone = Arc::clone(&reader_running);
                running_clone.store(true, Ordering::SeqCst);

                // Dedicated background reader thread
                std::thread::Builder::new()
                    .name("mpv-ipc-reader".into())
                    .spawn(move || {
                        let mut r = reader;
                        let mut line = String::new();
                        while running_clone.load(Ordering::SeqCst) {
                            line.clear();
                            match r.read_line(&mut line) {
                                Ok(0) => break, // EOF
                                Ok(_) => {
                                    let trimmed = line.trim();
                                    if trimmed.is_empty() {
                                        continue;
                                    }
                                    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                                        if let Some(req_id) =
                                            parsed.get("request_id").and_then(|v| v.as_u64())
                                        {
                                            let mut map = pending_clone.lock().unwrap();
                                            if let Some(tx) = map.remove(&req_id) {
                                                let _ = tx.send(parsed);
                                            }
                                        }
                                        // Asynchronous events (without request_id) pass through harmlessly
                                    }
                                }
                                Err(_) => break, // Pipe broken
                            }
                        }
                        running_clone.store(false, Ordering::SeqCst);
                    })
                    .map_err(|e| MpvError::Player(format!("Failed to spawn reader thread: {e}")))?;
            }
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
            #[cfg(windows)]
            job_handle,
        };

        // Query active decoder through IPC
        if player.reader_running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(250));
            match player.query_active_decoder() {
                Ok(decoder) => {
                    info!(
                        active_decoder = %decoder,
                        requested = ?launch_cfg.config.hardware_acceleration,
                        "Hardware decoding telemetry verified"
                    );
                    player.active_decoder = decoder;
                }
                Err(err) => {
                    warn!(%err, "Failed initial query for active decoder");
                }
            }
            player.state = PlaybackState::Playing;
        } else {
            warn!(
                "Could not connect to MPV IPC pipe within timeout; player running without live IPC"
            );
        }

        Ok(player)
    }

    #[cfg(windows)]
    fn attach_job_object(
        child: &Child,
    ) -> Result<Option<windows::Win32::Foundation::HANDLE>, MpvError> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let job = CreateJobObjectW(None, None)
                .map_err(|e| MpvError::WindowsApi(format!("CreateJobObject failed: {e}")))?;

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| MpvError::WindowsApi(format!("SetInformationJobObject failed: {e}")))?;

            let proc_handle = HANDLE(child.as_raw_handle() as *mut core::ffi::c_void);
            AssignProcessToJobObject(job, proc_handle).map_err(|e| {
                MpvError::WindowsApi(format!("AssignProcessToJobObject failed: {e}"))
            })?;

            Ok(Some(job))
        }
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

        match rx.recv_timeout(Duration::from_millis(1500)) {
            Ok(parsed) => {
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
            Err(_) => {
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
        info!("Stopping video player process");
        self.reader_running.store(false, Ordering::SeqCst);
        let _ = self.send_command(&[json!("quit")]);
        *self.pipe_writer.lock().unwrap() = None;

        let wait_deadline = Instant::now() + Duration::from_millis(150);
        while Instant::now() < wait_deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let _ = self.child.kill();
        let _ = self.child.wait();
        self.state = PlaybackState::Stopped;
        Ok(())
    }

    /// Queries working set memory bytes of the MPV child process.
    pub fn get_memory_bytes(&self) -> u64 {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
            };

            unsafe {
                let proc_handle = HANDLE(self.child.as_raw_handle() as *mut core::ffi::c_void);
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
        #[cfg(windows)]
        if let Some(h) = self.job_handle.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}
