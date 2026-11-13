//! Windows system event notification listener.
//!
//! Listens for:
//! 1. Session lock and unlock events (`WTSRegisterSessionNotification` / `WM_WTSSESSION_CHANGE`)
//! 2. Display power states (`RegisterPowerSettingNotification` with `GUID_CONSOLE_DISPLAY_STATE`)
//! 3. Display topology and resolution changes (`WM_DISPLAYCHANGE`)
//! 4. Windows Explorer restart events (`RegisterWindowMessageW("TaskbarCreated")`)
//!
//! Runs on a dedicated background thread with a Win32 message-only window (`HWND_MESSAGE`),
//! forwarding typed notifications over an unbounded channel to decouple platform message handling
//! from the wallpaper engine loop.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use tracing::{debug, error, info};

/// Events emitted by the Windows system notification listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPlatformEvent {
    /// Workstation has been locked.
    SessionLocked,
    /// Workstation has been unlocked.
    SessionUnlocked,
    /// Monitor display powered off or entering low-power standby.
    DisplayPowerOff,
    /// Monitor display powered on or active.
    DisplayPowerOn,
    /// Display resolution, topology, or orientation changed.
    DisplayTopologyChanged,
    /// Windows Explorer process restarted (taskbar recreated).
    ExplorerRestarted,
}

/// Handle to the system event listener. Drops stop the listener thread and clean up Win32 registrations.
pub struct SystemEventListener {
    rx: Receiver<SystemPlatformEvent>,
    #[cfg(windows)]
    thread_handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(windows)]
    hwnd_val: Arc<AtomicIsize>,
}

impl SystemEventListener {
    /// Creates and starts the background Windows system event listener.
    pub fn start() -> Result<Self, String> {
        let (tx, rx) = channel();

        #[cfg(windows)]
        {
            let hwnd_val = Arc::new(AtomicIsize::new(0));
            let hwnd_clone = Arc::clone(&hwnd_val);

            let (init_tx, init_rx) = channel();

            let handle = std::thread::Builder::new()
                .name("sys-event-listener".to_string())
                .spawn(move || {
                    platform::run_notification_thread(tx, hwnd_clone, init_tx);
                })
                .map_err(|e| format!("Failed to spawn notification thread: {}", e))?;

            // Await initialization of message window
            match init_rx.recv() {
                Ok(Ok(())) => Ok(Self {
                    rx,
                    thread_handle: Some(handle),
                    hwnd_val,
                }),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!(
                    "Notification thread initialization channel closed: {}",
                    e
                )),
            }
        }

        #[cfg(not(windows))]
        {
            Ok(Self { rx })
        }
    }

    /// Try to receive a pending system event without blocking.
    pub fn try_recv(&self) -> Option<SystemPlatformEvent> {
        self.rx.try_recv().ok()
    }
}

#[cfg(windows)]
impl Drop for SystemEventListener {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

        let val = self.hwnd_val.load(Ordering::SeqCst);
        if val != 0 {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{
        RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW,
        SetWindowLongPtrW, GWLP_USERDATA, MSG, REGISTER_NOTIFICATION_FLAGS, WM_CLOSE, WM_DESTROY,
        WM_DISPLAYCHANGE, WM_POWERBROADCAST, WNDCLASSEXW,
    };

    // Windows Message Constants
    const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    const WTS_SESSION_LOCK: usize = 0x7;
    const WTS_SESSION_UNLOCK: usize = 0x8;
    const PBT_POWERSETTINGCHANGE: usize = 0x8013;

    // GUID_CONSOLE_DISPLAY_STATE: {0017f640-c327-4330-834c-223f23c5b20d}
    const GUID_CONSOLE_DISPLAY_STATE: windows::core::GUID =
        windows::core::GUID::from_u128(0x0017f640_c327_4330_834c_223f23c5b20d);

    #[repr(C)]
    struct POWERBROADCAST_SETTING {
        power_setting: windows::core::GUID,
        data_length: u32,
        data: [u8; 1],
    }

    struct ListenerState {
        tx: Sender<SystemPlatformEvent>,
        taskbar_created_msg: u32,
        power_notify_handle: Option<HPOWERNOTIFY>,
    }

    const NOTIFY_WINDOW_CLASS: PCWSTR = w!("RustWallpaperNotificationWindow");
    static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

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

        let state = &mut *ptr;

        if msg == state.taskbar_created_msg && state.taskbar_created_msg != 0 {
            info!("Received TaskbarCreated message: Windows Explorer restarted");
            let _ = state.tx.send(SystemPlatformEvent::ExplorerRestarted);
            return LRESULT(0);
        }

        match msg {
            WM_WTSSESSION_CHANGE => {
                match wparam.0 {
                    WTS_SESSION_LOCK => {
                        info!("Received WTS_SESSION_LOCK: Workstation locked");
                        let _ = state.tx.send(SystemPlatformEvent::SessionLocked);
                    }
                    WTS_SESSION_UNLOCK => {
                        info!("Received WTS_SESSION_UNLOCK: Workstation unlocked");
                        let _ = state.tx.send(SystemPlatformEvent::SessionUnlocked);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                if wparam.0 == PBT_POWERSETTINGCHANGE && lparam.0 != 0 {
                    let setting = &*(lparam.0 as *const POWERBROADCAST_SETTING);
                    if setting.power_setting == GUID_CONSOLE_DISPLAY_STATE
                        && setting.data_length >= 4
                    {
                        let display_state = *(setting.data.as_ptr() as *const u32);
                        match display_state {
                            0 => {
                                info!("Display power state changed: OFF");
                                let _ = state.tx.send(SystemPlatformEvent::DisplayPowerOff);
                            }
                            1 => {
                                info!("Display power state changed: ON");
                                let _ = state.tx.send(SystemPlatformEvent::DisplayPowerOn);
                            }
                            2 => {
                                debug!("Display power state changed: DIMMED");
                            }
                            _ => {}
                        }
                    }
                }
                LRESULT(1)
            }
            WM_DISPLAYCHANGE => {
                info!("Received WM_DISPLAYCHANGE: Display geometry or topology changed");
                let _ = state.tx.send(SystemPlatformEvent::DisplayTopologyChanged);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(hpower) = state.power_notify_handle.take() {
                    let _ = UnregisterPowerSettingNotification(hpower);
                }
                let _ = WTSUnRegisterSessionNotification(hwnd);
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
    ) {
        unsafe {
            let hinstance = match GetModuleHandleW(PCWSTR::null()) {
                Ok(h) => h,
                Err(e) => {
                    let _ = init_tx.send(Err(format!("GetModuleHandleW failed: {}", e)));
                    return;
                }
            };

            if !CLASS_REGISTERED.load(Ordering::Acquire) {
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
                    lpfnWndProc: Some(notify_wndproc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinstance.into(),
                    hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
                    hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
                    hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                    lpszMenuName: PCWSTR::null(),
                    lpszClassName: NOTIFY_WINDOW_CLASS,
                    hIconSm: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
                };

                let atom = RegisterClassExW(&wc);
                if atom == 0 {
                    let err = windows::core::Error::from_win32();
                    if err.code().0 != (0x80070582u32 as i32) && err.code().0 != 1410 {
                        let _ = init_tx.send(Err(format!("RegisterClassExW failed: {}", err)));
                        return;
                    }
                }
                CLASS_REGISTERED.store(true, Ordering::Release);
            }

            // Register TaskbarCreated message for Explorer recovery
            let taskbar_msg = RegisterWindowMessageW(w!("TaskbarCreated"));

            // Create hidden message-only window
            // HWND_MESSAGE is ((HWND)-3)
            let hwnd_message = HWND(-3isize as *mut core::ffi::c_void);

            let hwnd = match CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                NOTIFY_WINDOW_CLASS,
                w!("WallpaperPlatformNotifier"),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                hwnd_message,
                windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                hinstance,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = init_tx.send(Err(format!(
                        "CreateWindowExW for notification window failed: {}",
                        e
                    )));
                    return;
                }
            };

            hwnd_holder.store(hwnd.0 as isize, Ordering::SeqCst);

            // Register for Session Lock/Unlock notifications
            if let Err(e) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
                error!(%e, "WTSRegisterSessionNotification failed");
            }

            // Register for Console Display Power notifications
            let power_notify = match RegisterPowerSettingNotification(
                HANDLE(hwnd.0),
                &GUID_CONSOLE_DISPLAY_STATE,
                REGISTER_NOTIFICATION_FLAGS(0), // DEVICE_NOTIFY_WINDOW_HANDLE
            ) {
                Ok(h) => Some(h),
                Err(e) => {
                    error!(%e, "RegisterPowerSettingNotification failed");
                    None
                }
            };

            let state = Box::new(ListenerState {
                tx,
                taskbar_created_msg: taskbar_msg,
                power_notify_handle: power_notify,
            });

            let state_ptr = Box::into_raw(state);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

            // Notify parent thread of success
            let _ = init_tx.send(Ok(()));

            debug!("Notification message loop starting");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            debug!("Notification message loop exited");

            // Clean up state
            let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut ListenerState;
            if !ptr.is_null() {
                let _ = Box::from_raw(ptr);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_event_listener_lifecycle() {
        let listener = SystemEventListener::start();
        assert!(listener.is_ok(), "Failed to start SystemEventListener");

        let listener = listener.unwrap();
        // Initially queue should be empty
        assert_eq!(listener.try_recv(), None);
        // Clean drop
        drop(listener);
    }
}
