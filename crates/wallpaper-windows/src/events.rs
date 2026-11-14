//! Windows system event notification listener.
//!
//! Listens for:
//! 1. Session lock and unlock (`WTSRegisterSessionNotification` / `WM_WTSSESSION_CHANGE`)
//! 2. Display power state (`RegisterPowerSettingNotification` with `GUID_CONSOLE_DISPLAY_STATE`)
//! 3. System suspend / resume (`WM_POWERBROADCAST`: `PBT_APMSUSPEND`, `PBT_APMRESUME*`)
//! 4. Display topology and resolution changes (`WM_DISPLAYCHANGE`)
//! 5. Windows Explorer restarts (the registered `TaskbarCreated` broadcast)
//!
//! # Threading
//!
//! The listener owns a dedicated thread with a hidden window and its own message loop. It
//! only pushes small values into a channel: it never touches players and never waits on MPV
//! IPC, so a slow player can never delay (or lose) a system notification.
//!
//! # Why a hidden *top-level* window and not `HWND_MESSAGE`
//!
//! `WM_DISPLAYCHANGE` and `TaskbarCreated` are *broadcast* messages. Message-only windows do
//! not receive broadcasts, so with a message-only window topology changes and Explorer
//! restarts would silently never arrive. The window here is an invisible, non-activating tool
//! window: it receives broadcasts but is never shown, never in the taskbar, never focused.
//!
//! # Duplicate suppression
//!
//! Windows re-delivers state (e.g. `PBT_APMRESUMEAUTOMATIC` and `PBT_APMRESUMESUSPEND` on the
//! same wake). The listener remembers the last value emitted per state-like event and drops
//! exact repeats. Consumers must still treat events idempotently.

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use wallpaper_core::registration::{Coverage, RegistrationReport, RetryPolicy};
use wallpaper_core::runtime::RuntimeEvent;

#[cfg(windows)]
use std::sync::atomic::{AtomicIsize, Ordering};
#[cfg(windows)]
use std::sync::mpsc::Sender;
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use tracing::{debug, error, info};

/// Events emitted by the Windows system notification listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPlatformEvent {
    SessionLocked,
    SessionUnlocked,
    DisplayPowerOff,
    DisplayPowerOn,
    /// System is about to suspend (sleep / hibernate).
    SystemSuspend,
    /// System resumed from suspend.
    SystemResume,
    DisplayTopologyChanged,
    ExplorerRestarted,
}

impl From<SystemPlatformEvent> for RuntimeEvent {
    fn from(event: SystemPlatformEvent) -> Self {
        match event {
            SystemPlatformEvent::SessionLocked => RuntimeEvent::SessionLocked,
            SystemPlatformEvent::SessionUnlocked => RuntimeEvent::SessionUnlocked,
            SystemPlatformEvent::DisplayPowerOff => RuntimeEvent::DisplayPowerOff,
            SystemPlatformEvent::DisplayPowerOn => RuntimeEvent::DisplayPowerOn,
            SystemPlatformEvent::SystemSuspend => RuntimeEvent::SystemSuspend,
            SystemPlatformEvent::SystemResume => RuntimeEvent::SystemResume,
            SystemPlatformEvent::DisplayTopologyChanged => RuntimeEvent::DisplayTopologyChanged,
            SystemPlatformEvent::ExplorerRestarted => RuntimeEvent::ExplorerRestarted,
        }
    }
}

/// Result of polling the listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerPoll {
    Event(SystemPlatformEvent),
    /// Nothing pending; the listener is healthy.
    Empty,
    /// The listener thread has exited (panic or failure). The owner should restart it.
    Disconnected,
}

/// Test hook: number of upcoming registration attempts to fail artificially, per source.
/// `u32::MAX` means "always fail". All zero (the default) is production behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InjectedFailures {
    pub session_notifications: u32,
    pub display_power: u32,
    pub explorer_restart: u32,
}

/// Listener configuration. `Default` is what the daemon uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListenerOptions {
    /// Bounded retry schedule for registrations that fail.
    pub retry: RetryPolicy,
    pub inject_failures: InjectedFailures,
}

/// Handle to the system event listener. Dropping it stops the thread and removes every Win32
/// registration (session notification, power-setting notification, window, window class).
pub struct SystemEventListener {
    rx: Receiver<SystemPlatformEvent>,
    #[cfg(windows)]
    thread_handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    hwnd_val: Arc<AtomicIsize>,
    #[cfg(windows)]
    report: Arc<Mutex<RegistrationReport>>,
    #[cfg(not(windows))]
    _keepalive: std::sync::mpsc::Sender<SystemPlatformEvent>,
}

impl SystemEventListener {
    /// Creates and starts the background Windows system event listener.
    pub fn start() -> Result<Self, String> {
        Self::start_with(ListenerOptions::default())
    }

    /// Like [`start`](Self::start) with explicit options (retry schedule, fault injection).
    ///
    /// Only failing to create the hidden window fails the start. A registration that fails is
    /// retried in the background and reported through
    /// [`registration_report`](Self::registration_report); it never silently looks like success.
    pub fn start_with(options: ListenerOptions) -> Result<Self, String> {
        #[allow(unused_variables)]
        let (tx, rx) = channel();
        #[cfg(not(windows))]
        let _ = options;

        #[cfg(windows)]
        {
            let hwnd_val = Arc::new(AtomicIsize::new(0));
            let hwnd_clone = Arc::clone(&hwnd_val);
            let report = Arc::new(Mutex::new(RegistrationReport::default()));
            let report_clone = Arc::clone(&report);
            let (init_tx, init_rx) = channel();

            let handle = std::thread::Builder::new()
                .name("sys-event-listener".to_string())
                .spawn(move || {
                    platform::run_notification_thread(
                        tx,
                        hwnd_clone,
                        init_tx,
                        options,
                        report_clone,
                    );
                })
                .map_err(|e| format!("Failed to spawn notification thread: {}", e))?;

            match init_rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    rx,
                    thread_handle: Some(handle),
                    hwnd_val,
                    report,
                }),
                Ok(Err(e)) => {
                    let _ = handle.join();
                    Err(e)
                }
                Err(e) => {
                    let _ = handle.join();
                    Err(format!(
                        "Notification thread initialization channel closed: {}",
                        e
                    ))
                }
            }
        }

        #[cfg(not(windows))]
        {
            Ok(Self { rx, _keepalive: tx })
        }
    }

    /// Current registration status of every event source. Cheap; safe to call every tick.
    pub fn registration_report(&self) -> RegistrationReport {
        #[cfg(windows)]
        {
            self.report
                .lock()
                .map(|r| r.clone())
                .unwrap_or_else(|p| p.into_inner().clone())
        }
        #[cfg(not(windows))]
        {
            RegistrationReport::default()
        }
    }

    /// Whether the listener can currently hear about *everything* the runtime depends on.
    /// `Full` only if every registration is active.
    pub fn coverage(&self) -> Coverage {
        self.registration_report().coverage()
    }

    /// Polls without blocking, distinguishing "nothing yet" from "listener died".
    pub fn poll(&self) -> ListenerPoll {
        match self.rx.try_recv() {
            Ok(event) => ListenerPoll::Event(event),
            Err(TryRecvError::Empty) => ListenerPoll::Empty,
            Err(TryRecvError::Disconnected) => ListenerPoll::Disconnected,
        }
    }

    /// Try to receive a pending system event without blocking.
    pub fn try_recv(&self) -> Option<SystemPlatformEvent> {
        match self.poll() {
            ListenerPoll::Event(event) => Some(event),
            _ => None,
        }
    }

    /// Drains every pending event in arrival order. Returns `(events, still_alive)`.
    pub fn drain(&self) -> (Vec<SystemPlatformEvent>, bool) {
        let mut events = Vec::new();
        loop {
            match self.poll() {
                ListenerPoll::Event(event) => events.push(event),
                ListenerPoll::Empty => return (events, true),
                ListenerPoll::Disconnected => return (events, false),
            }
        }
    }

    /// Raw handle of the hidden window, for tests that inject window messages.
    #[cfg(all(windows, test))]
    fn window_handle_for_test(&self) -> isize {
        self.hwnd_val.load(Ordering::SeqCst)
    }
}

#[cfg(windows)]
impl Drop for SystemEventListener {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

        let val = self.hwnd_val.load(Ordering::SeqCst);
        if val != 0 {
            // PostMessage is safe from any thread; the listener thread destroys its own window.
            unsafe {
                let hwnd = HWND(val as *mut core::ffi::c_void);
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::time::Instant;
    use wallpaper_core::registration::{EventSource, RegistrationTracker, RetryDecision};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{
        RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, KillTimer, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
        SetTimer, SetWindowLongPtrW, UnregisterClassW, GWLP_USERDATA, HMENU, MSG,
        REGISTER_NOTIFICATION_FLAGS, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_POWERBROADCAST,
        WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    // Values verified against the `windows` crate's generated Win32 metadata.
    pub(super) const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    pub(super) const WTS_SESSION_LOCK: usize = 0x7;
    pub(super) const WTS_SESSION_UNLOCK: usize = 0x8;
    pub(super) const PBT_APMSUSPEND: usize = 0x0004;
    pub(super) const PBT_APMRESUMECRITICAL: usize = 0x0006;
    pub(super) const PBT_APMRESUMESUSPEND: usize = 0x0007;
    pub(super) const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
    pub(super) const PBT_POWERSETTINGCHANGE: usize = 0x8013;

    /// `GUID_CONSOLE_DISPLAY_STATE` = {6fe69556-704a-47a0-8f24-c28d936fda47}.
    ///
    /// NOTE: earlier revisions used {0017f640-c327-4330-834c-223f23c5b20d}, which is not a
    /// Windows power-setting GUID. Registering for an unknown GUID succeeds but never fires,
    /// so display power events were never delivered.
    pub(super) const GUID_CONSOLE_DISPLAY_STATE: windows::core::GUID =
        windows::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

    /// Layout of `POWERBROADCAST_SETTING` (`data` is a variable-length tail).
    #[repr(C)]
    pub(super) struct PowerBroadcastSetting {
        pub power_setting: windows::core::GUID,
        pub data_length: u32,
        pub data: [u8; 1],
    }

    /// Timer id used to drive registration retries on the listener's own window.
    const RETRY_TIMER_ID: usize = 1;

    struct ListenerState {
        tx: Sender<SystemPlatformEvent>,
        taskbar_created_msg: u32,
        power_notify_handle: Option<HPOWERNOTIFY>,
        wts_registered: bool,
        last_locked: Option<bool>,
        last_display_on: Option<bool>,
        last_suspended: Option<bool>,
        tracker: RegistrationTracker,
        shared_report: Arc<Mutex<RegistrationReport>>,
        inject: InjectedFailures,
        timer_active: bool,
    }

    impl ListenerState {
        fn emit(&self, event: SystemPlatformEvent) {
            if self.tx.send(event).is_err() {
                debug!(?event, "Event receiver dropped; discarding event");
            }
        }

        fn set_locked(&mut self, locked: bool) {
            if self.last_locked == Some(locked) {
                debug!(locked, "Duplicate session lock state suppressed");
                return;
            }
            self.last_locked = Some(locked);
            self.emit(if locked {
                SystemPlatformEvent::SessionLocked
            } else {
                SystemPlatformEvent::SessionUnlocked
            });
        }

        fn set_display_on(&mut self, on: bool) {
            if self.last_display_on == Some(on) {
                debug!(on, "Duplicate display power state suppressed");
                return;
            }
            self.last_display_on = Some(on);
            self.emit(if on {
                SystemPlatformEvent::DisplayPowerOn
            } else {
                SystemPlatformEvent::DisplayPowerOff
            });
        }

        fn set_suspended(&mut self, suspended: bool) {
            if self.last_suspended == Some(suspended) {
                debug!(suspended, "Duplicate system suspend state suppressed");
                return;
            }
            self.last_suspended = Some(suspended);
            self.emit(if suspended {
                SystemPlatformEvent::SystemSuspend
            } else {
                SystemPlatformEvent::SystemResume
            });
        }
    }

    /// Consumes one injected failure for a source, if any remain.
    fn take_injected(counter: &mut u32) -> bool {
        if *counter > 0 {
            if *counter != u32::MAX {
                *counter -= 1;
            }
            true
        } else {
            false
        }
    }

    /// Makes one registration attempt for `source` and records the outcome.
    ///
    /// Works through a raw pointer and never holds a `&mut ListenerState` across a Win32 call:
    /// Windows may deliver the initial notification (e.g. the current display state)
    /// synchronously from inside the registration call, re-entering the window procedure.
    unsafe fn attempt(ptr: *mut ListenerState, hwnd: HWND, source: EventSource, now: Instant) {
        // Phase 1: decide whether to inject a failure (short borrow).
        let injected = {
            let st = &mut *ptr;
            match source {
                EventSource::SessionNotifications => {
                    take_injected(&mut st.inject.session_notifications)
                }
                EventSource::DisplayPower => take_injected(&mut st.inject.display_power),
                EventSource::ExplorerRestart => take_injected(&mut st.inject.explorer_restart),
                EventSource::SuspendResume | EventSource::DisplayTopology => false,
            }
        };

        // Phase 2: the Win32 call, with no live borrow.
        enum Outcome {
            Ok,
            Power(HPOWERNOTIFY),
            Message(u32),
            Err(String),
        }
        let outcome = if injected {
            Outcome::Err("injected failure (test hook)".to_string())
        } else {
            match source {
                EventSource::SessionNotifications => {
                    match WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
                        Ok(()) => Outcome::Ok,
                        Err(e) => Outcome::Err(e.to_string()),
                    }
                }
                EventSource::DisplayPower => {
                    // Windows sends the current display state right after registration.
                    match RegisterPowerSettingNotification(
                        HANDLE(hwnd.0),
                        &GUID_CONSOLE_DISPLAY_STATE,
                        REGISTER_NOTIFICATION_FLAGS(0), // DEVICE_NOTIFY_WINDOW_HANDLE
                    ) {
                        Ok(h) => Outcome::Power(h),
                        Err(e) => Outcome::Err(e.to_string()),
                    }
                }
                EventSource::ExplorerRestart => {
                    // Explorer announces a fresh taskbar by broadcasting this registered message.
                    let id = RegisterWindowMessageW(w!("TaskbarCreated"));
                    if id == 0 {
                        Outcome::Err("RegisterWindowMessageW(TaskbarCreated) returned 0".into())
                    } else {
                        Outcome::Message(id)
                    }
                }
                // Delivered to any live top-level window; no registration exists to fail.
                EventSource::SuspendResume | EventSource::DisplayTopology => Outcome::Ok,
            }
        };

        // Phase 3: record (short borrow).
        let st = &mut *ptr;
        match outcome {
            Outcome::Ok | Outcome::Power(_) | Outcome::Message(_) => {
                match outcome {
                    Outcome::Power(h) => st.power_notify_handle = Some(h),
                    Outcome::Message(id) => st.taskbar_created_msg = id,
                    _ => {}
                }
                if source == EventSource::SessionNotifications {
                    st.wts_registered = true;
                }
                st.tracker.mark_active(source);
                debug!(source = source.as_str(), "Event source registered");
            }
            Outcome::Err(error) => match st.tracker.record_failure(source, error.clone(), now) {
                RetryDecision::RetryAfter(delay) => {
                    error!(source = source.as_str(), %error, ?delay, "Event registration failed; will retry");
                }
                RetryDecision::GiveUp => {
                    error!(source = source.as_str(), %error, "Event registration FAILED permanently; events from this source will not arrive");
                }
            },
        }
    }

    /// Publishes the tracker's report to the owner and starts/stops the retry timer as needed.
    unsafe fn publish_and_sync_timer(ptr: *mut ListenerState, hwnd: HWND) {
        let st = &mut *ptr;
        if let Ok(mut shared) = st.shared_report.lock() {
            *shared = st.tracker.report().clone();
        }
        let need = st.tracker.has_pending_retries();
        if need && !st.timer_active {
            let ms = (st.tracker.policy().initial_delay.as_millis() as u32 / 2).clamp(10, 250);
            SetTimer(hwnd, RETRY_TIMER_ID, ms, None);
            st.timer_active = true;
        } else if !need && st.timer_active {
            let _ = KillTimer(hwnd, RETRY_TIMER_ID);
            st.timer_active = false;
        }
    }

    unsafe fn run_due_retries(ptr: *mut ListenerState, hwnd: HWND, now: Instant) {
        let due = (*ptr).tracker.due(now);
        for source in due {
            attempt(ptr, hwnd, source, now);
        }
        publish_and_sync_timer(ptr, hwnd);
    }

    const NOTIFY_WINDOW_CLASS: PCWSTR = w!("RustWallpaperNotificationWindow");

    /// Live listeners sharing the window class: registered by the first, unregistered by the last.
    static CLASS_REFS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
    const ERROR_CLASS_ALREADY_EXISTS: i32 = 1410;

    unsafe fn acquire_class(hinstance: HINSTANCE) -> Result<(), String> {
        let mut refs = CLASS_REFS.lock().unwrap_or_else(|p| p.into_inner());
        if *refs == 0 {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(notify_wndproc),
                hInstance: hinstance,
                lpszClassName: NOTIFY_WINDOW_CLASS,
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                let err = windows::core::Error::from_win32();
                if (err.code().0 & 0xFFFF) != ERROR_CLASS_ALREADY_EXISTS {
                    return Err(format!("RegisterClassExW failed: {}", err));
                }
            }
        }
        *refs += 1;
        Ok(())
    }

    unsafe fn release_class(hinstance: HINSTANCE) {
        let mut refs = CLASS_REFS.lock().unwrap_or_else(|p| p.into_inner());
        *refs = refs.saturating_sub(1);
        if *refs == 0 {
            if let Err(e) = UnregisterClassW(NOTIFY_WINDOW_CLASS, hinstance) {
                debug!(%e, "UnregisterClassW failed");
            }
        }
    }

    unsafe extern "system" fn notify_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut ListenerState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        // Handled before taking a long-lived `&mut`, because retries re-derive from `ptr`.
        if msg == WM_TIMER && wparam.0 == RETRY_TIMER_ID {
            run_due_retries(ptr, hwnd, Instant::now());
            return LRESULT(0);
        }

        let state = &mut *ptr;

        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
            info!("Received TaskbarCreated: Windows Explorer restarted");
            state.emit(SystemPlatformEvent::ExplorerRestarted);
            return LRESULT(0);
        }

        match msg {
            WM_WTSSESSION_CHANGE => {
                match wparam.0 {
                    WTS_SESSION_LOCK => {
                        info!("Received WTS_SESSION_LOCK: workstation locked");
                        state.set_locked(true);
                    }
                    WTS_SESSION_UNLOCK => {
                        info!("Received WTS_SESSION_UNLOCK: workstation unlocked");
                        state.set_locked(false);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                match wparam.0 {
                    PBT_APMSUSPEND => {
                        info!("Received PBT_APMSUSPEND: system suspending");
                        state.set_suspended(true);
                    }
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND | PBT_APMRESUMECRITICAL => {
                        info!(code = wparam.0, "Received resume notification");
                        state.set_suspended(false);
                    }
                    PBT_POWERSETTINGCHANGE if lparam.0 != 0 => {
                        let setting = &*(lparam.0 as *const PowerBroadcastSetting);
                        if setting.power_setting == GUID_CONSOLE_DISPLAY_STATE
                            && setting.data_length >= 4
                        {
                            let value =
                                std::ptr::read_unaligned(setting.data.as_ptr() as *const u32);
                            match value {
                                0 => {
                                    info!("Display power state changed: OFF");
                                    state.set_display_on(false);
                                }
                                1 => {
                                    info!("Display power state changed: ON");
                                    state.set_display_on(true);
                                }
                                // 2 = dimmed: the display still shows the desktop.
                                _ => debug!(value, "Display power state: DIMMED/other"),
                            }
                        }
                    }
                    _ => {}
                }
                LRESULT(1) // TRUE: grant the request
            }
            WM_DISPLAYCHANGE => {
                info!("Received WM_DISPLAYCHANGE: display geometry or topology changed");
                state.emit(SystemPlatformEvent::DisplayTopologyChanged);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if state.timer_active {
                    let _ = KillTimer(hwnd, RETRY_TIMER_ID);
                    state.timer_active = false;
                }
                if let Some(hpower) = state.power_notify_handle.take() {
                    let _ = UnregisterPowerSettingNotification(hpower);
                }
                if state.wts_registered {
                    state.wts_registered = false;
                    let _ = WTSUnRegisterSessionNotification(hwnd);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub fn run_notification_thread(
        tx: Sender<SystemPlatformEvent>,
        hwnd_holder: Arc<AtomicIsize>,
        init_tx: Sender<Result<(), String>>,
        options: ListenerOptions,
        shared_report: Arc<Mutex<RegistrationReport>>,
    ) {
        unsafe {
            let hinstance: HINSTANCE = match GetModuleHandleW(PCWSTR::null()) {
                Ok(h) => h.into(),
                Err(e) => {
                    let _ = init_tx.send(Err(format!("GetModuleHandleW failed: {}", e)));
                    return;
                }
            };

            if let Err(e) = acquire_class(hinstance) {
                let _ = init_tx.send(Err(e));
                return;
            }

            // Hidden TOP-LEVEL window (see module docs).
            let hwnd = match CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                NOTIFY_WINDOW_CLASS,
                w!("WallpaperPlatformNotifier"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    release_class(hinstance);
                    let _ = init_tx.send(Err(format!(
                        "CreateWindowExW for notification window failed: {}",
                        e
                    )));
                    return;
                }
            };

            let state = Box::new(ListenerState {
                tx,
                taskbar_created_msg: 0,
                power_notify_handle: None,
                wts_registered: false,
                last_locked: None,
                last_display_on: None,
                last_suspended: None,
                tracker: RegistrationTracker::new(options.retry),
                shared_report,
                inject: options.inject_failures,
                timer_active: false,
            });
            let ptr = Box::into_raw(state);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as isize);

            // First attempt for every source. A failure here does not fail the start: it is
            // recorded, published and retried on the window's timer.
            let now = Instant::now();
            for source in EventSource::ALL {
                attempt(ptr, hwnd, source, now);
            }
            publish_and_sync_timer(ptr, hwnd);

            hwnd_holder.store(hwnd.0 as isize, Ordering::SeqCst);
            let _ = init_tx.send(Ok(()));

            debug!("Notification message loop starting");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            debug!("Notification message loop exited");

            hwnd_holder.store(0, Ordering::SeqCst);
            let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut ListenerState;
            if !ptr.is_null() {
                drop(Box::from_raw(ptr));
            }
            release_class(hinstance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_event_maps_to_runtime_event() {
        let pairs = [
            (
                SystemPlatformEvent::SessionLocked,
                RuntimeEvent::SessionLocked,
            ),
            (
                SystemPlatformEvent::SessionUnlocked,
                RuntimeEvent::SessionUnlocked,
            ),
            (
                SystemPlatformEvent::DisplayPowerOff,
                RuntimeEvent::DisplayPowerOff,
            ),
            (
                SystemPlatformEvent::DisplayPowerOn,
                RuntimeEvent::DisplayPowerOn,
            ),
            (
                SystemPlatformEvent::SystemSuspend,
                RuntimeEvent::SystemSuspend,
            ),
            (
                SystemPlatformEvent::SystemResume,
                RuntimeEvent::SystemResume,
            ),
            (
                SystemPlatformEvent::DisplayTopologyChanged,
                RuntimeEvent::DisplayTopologyChanged,
            ),
            (
                SystemPlatformEvent::ExplorerRestarted,
                RuntimeEvent::ExplorerRestarted,
            ),
        ];
        for (platform, runtime) in pairs {
            assert_eq!(RuntimeEvent::from(platform), runtime);
        }
    }

    #[test]
    fn test_system_event_listener_lifecycle() {
        let listener = SystemEventListener::start().expect("listener starts");
        let (_, alive) = listener.drain();
        assert!(
            alive,
            "listener thread must be alive while the handle exists"
        );
        drop(listener);
    }

    #[test]
    fn listener_can_be_restarted_after_drop() {
        // Exercises window-class refcounting: register, unregister, register again.
        for _ in 0..3 {
            drop(SystemEventListener::start().expect("listener starts"));
        }
    }
}

/// Tests that inject real window messages into the listener's window procedure.
/// They run only on a Windows target (real Windows, or Wine).
#[cfg(all(windows, test))]
mod windows_integration_tests {
    use super::platform::*;
    use super::*;
    use std::time::Duration;
    use windows::core::w;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, RegisterWindowMessageW, SendMessageW, WM_DISPLAYCHANGE, WM_POWERBROADCAST,
    };

    fn send(l: &SystemEventListener, msg: u32, wparam: usize, lparam: isize) {
        let hwnd = HWND(l.window_handle_for_test() as *mut core::ffi::c_void);
        // Synchronous: returns once the listener thread has handled the message.
        unsafe {
            SendMessageW(hwnd, msg, WPARAM(wparam), LPARAM(lparam));
        }
    }

    fn drain(l: &SystemEventListener) -> Vec<SystemPlatformEvent> {
        let (events, alive) = l.drain();
        assert!(alive, "listener thread must still be alive");
        events
    }

    #[repr(C)]
    struct DisplayStateMessage {
        guid: windows::core::GUID,
        len: u32,
        value: u32,
    }

    fn send_power_setting(l: &SystemEventListener, guid: windows::core::GUID, value: u32) {
        let msg = DisplayStateMessage {
            guid,
            len: 4,
            value,
        };
        send(
            l,
            WM_POWERBROADCAST,
            PBT_POWERSETTINGCHANGE,
            &msg as *const DisplayStateMessage as isize,
        );
    }

    #[test]
    fn session_lock_and_unlock_are_reported_once() {
        let l = SystemEventListener::start().unwrap();
        send(&l, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, 0);
        send(&l, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK, 0);
        send(&l, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK, 0);
        send(&l, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK, 0);
        assert_eq!(
            drain(&l),
            vec![
                SystemPlatformEvent::SessionLocked,
                SystemPlatformEvent::SessionUnlocked
            ]
        );
    }

    #[test]
    fn suspend_and_every_resume_flavour_are_reported() {
        let l = SystemEventListener::start().unwrap();
        send(&l, WM_POWERBROADCAST, PBT_APMSUSPEND, 0);
        send(&l, WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC, 0);
        send(&l, WM_POWERBROADCAST, PBT_APMRESUMESUSPEND, 0); // same wake: duplicate
        assert_eq!(
            drain(&l),
            vec![
                SystemPlatformEvent::SystemSuspend,
                SystemPlatformEvent::SystemResume
            ]
        );
        send(&l, WM_POWERBROADCAST, PBT_APMSUSPEND, 0);
        send(&l, WM_POWERBROADCAST, PBT_APMRESUMECRITICAL, 0);
        assert_eq!(
            drain(&l),
            vec![
                SystemPlatformEvent::SystemSuspend,
                SystemPlatformEvent::SystemResume
            ]
        );
    }

    #[test]
    fn display_power_states_are_decoded() {
        let l = SystemEventListener::start().unwrap();
        send_power_setting(&l, GUID_CONSOLE_DISPLAY_STATE, 0);
        send_power_setting(&l, GUID_CONSOLE_DISPLAY_STATE, 0); // duplicate
        send_power_setting(&l, GUID_CONSOLE_DISPLAY_STATE, 2); // dimmed: no event
        send_power_setting(&l, GUID_CONSOLE_DISPLAY_STATE, 1);
        assert_eq!(
            drain(&l),
            vec![
                SystemPlatformEvent::DisplayPowerOff,
                SystemPlatformEvent::DisplayPowerOn
            ]
        );
    }

    #[test]
    fn foreign_power_setting_guid_is_ignored() {
        let l = SystemEventListener::start().unwrap();
        let other = windows::core::GUID::from_u128(0x12345678_1234_1234_1234_123456789abc);
        send_power_setting(&l, other, 0);
        assert!(drain(&l).is_empty());
    }

    #[test]
    fn display_change_and_taskbar_created_are_reported() {
        let l = SystemEventListener::start().unwrap();
        send(&l, WM_DISPLAYCHANGE, 32, 0);
        let taskbar = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        assert_ne!(taskbar, 0);
        send(&l, taskbar, 0, 0);
        assert_eq!(
            drain(&l),
            vec![
                SystemPlatformEvent::DisplayTopologyChanged,
                SystemPlatformEvent::ExplorerRestarted
            ]
        );
    }

    #[test]
    fn listener_window_is_enumerable_so_it_receives_broadcasts() {
        // Message-only windows are NOT returned by EnumWindows and get no broadcasts.
        struct Search {
            target: isize,
            found: bool,
        }
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = &mut *(lparam.0 as *mut Search);
            if hwnd.0 as isize == search.target {
                search.found = true;
                return BOOL(0);
            }
            BOOL(1)
        }
        let l = SystemEventListener::start().unwrap();
        let mut search = Search {
            target: l.window_handle_for_test(),
            found: false,
        };
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut search as *mut Search as isize));
        }
        assert!(
            search.found,
            "listener window must be a real top-level window"
        );
    }

    #[test]
    fn shutdown_completes_promptly_even_after_a_burst() {
        let l = SystemEventListener::start().unwrap();
        for _ in 0..50 {
            send(&l, WM_DISPLAYCHANGE, 0, 0);
        }
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(l);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "drop must join the listener thread"
        );
    }

    // ---- registration status and retries ---------------------------------------------------

    use wallpaper_core::registration::{EventSource, RegistrationState};

    fn wait_for(secs: u64, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    #[test]
    fn registration_report_is_settled_and_consistent_after_start() {
        let l = SystemEventListener::start().unwrap();
        let report = l.registration_report();
        for source in EventSource::ALL {
            assert_ne!(
                report.state(source),
                &RegistrationState::Pending,
                "{source:?} was attempted during start"
            );
        }
        // These cannot fail on a live top-level window.
        for source in [
            EventSource::SuspendResume,
            EventSource::DisplayTopology,
            EventSource::ExplorerRestart,
        ] {
            assert_eq!(
                report.state(source),
                &RegistrationState::Active,
                "{source:?}"
            );
        }
        // Full coverage means exactly "every source active", never anything weaker.
        let all_active = EventSource::ALL
            .iter()
            .all(|s| report.state(*s) == &RegistrationState::Active);
        assert_eq!(report.is_full(), all_active);
        assert_eq!(l.coverage() == Coverage::Full, all_active);
    }

    #[test]
    fn transient_registration_failures_are_retried_until_active() {
        let options = ListenerOptions {
            retry: RetryPolicy {
                max_attempts: 6,
                initial_delay: Duration::from_millis(150),
                max_delay: Duration::from_millis(300),
            },
            inject_failures: InjectedFailures {
                session_notifications: 2,
                ..Default::default()
            },
        };
        let l = SystemEventListener::start_with(options).unwrap();

        // Straight after start the first attempt has failed: not full coverage, and the
        // report says why instead of pretending.
        let early = l.registration_report();
        assert!(matches!(
            early.state(EventSource::SessionNotifications),
            RegistrationState::Retrying { .. }
        ));
        assert!(
            !early.is_full(),
            "partial registration must not look like full coverage"
        );
        assert!(early
            .diagnostics()
            .iter()
            .any(|d| d.contains("session_notifications") && d.contains("retrying")));

        assert!(
            wait_for(6, || l
                .registration_report()
                .state(EventSource::SessionNotifications)
                == &RegistrationState::Active),
            "the third attempt succeeds on its own"
        );
    }

    #[test]
    fn permanently_failing_registration_stays_visibly_partial() {
        let options = ListenerOptions {
            retry: RetryPolicy {
                max_attempts: 3,
                initial_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(40),
            },
            inject_failures: InjectedFailures {
                explorer_restart: u32::MAX,
                ..Default::default()
            },
        };
        let l = SystemEventListener::start_with(options).unwrap();
        assert!(wait_for(6, || matches!(
            l.registration_report().state(EventSource::ExplorerRestart),
            RegistrationState::Failed { .. }
        )));

        let report = l.registration_report();
        assert!(matches!(
            report.state(EventSource::ExplorerRestart),
            RegistrationState::Failed { attempts: 3, .. }
        ));
        assert!(!report.is_full());
        assert!(matches!(
            report.coverage(),
            Coverage::Partial { ref missing } if missing.contains(&EventSource::ExplorerRestart)
        ));
        assert!(report.diagnostics().iter().any(|d| d.contains("FAILED")));

        // The sources that did register keep working: the listener is degraded, not dead.
        send(&l, WM_DISPLAYCHANGE, 0, 0);
        assert_eq!(drain(&l), vec![SystemPlatformEvent::DisplayTopologyChanged]);
    }

    #[test]
    fn drop_with_a_retry_timer_pending_completes_promptly() {
        let options = ListenerOptions {
            retry: RetryPolicy {
                max_attempts: 6,
                initial_delay: Duration::from_secs(5),
                max_delay: Duration::from_secs(8),
            },
            inject_failures: InjectedFailures {
                display_power: u32::MAX,
                ..Default::default()
            },
        };
        let l = SystemEventListener::start_with(options).unwrap();
        assert!(matches!(
            l.registration_report().state(EventSource::DisplayPower),
            RegistrationState::Retrying { .. }
        ));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(l);
            let _ = done_tx.send(());
        });
        assert!(done_rx.recv_timeout(Duration::from_secs(5)).is_ok());
    }
}
