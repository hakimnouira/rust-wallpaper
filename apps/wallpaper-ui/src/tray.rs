//! Native Windows notification-area icon.
//!
//! The tray icon owns a small message-only window so it can receive shell
//! callbacks without depending on eframe/winit internals. The UI polls
//! [`TrayAction`] values and keeps the icon alive for the lifetime of the app.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, LoadImageW, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu,
    TranslateMessage, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE,
    MF_SEPARATOR, MF_STRING, MSG, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_STYLE, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};

const TRAY_CALLBACK: u32 = WM_APP + 1;
const MENU_SHOW: usize = 1;
const MENU_QUIT: usize = 2;
const MENU_NEXT: usize = 3;
const MENU_TOGGLE_PAUSE: usize = 4;
const MENU_TOGGLE_MUTE: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Quit,
    NextWallpaper,
    TogglePause,
    ToggleMute,
}

struct TrayState {
    sender: Sender<TrayAction>,
    ctx: eframe::egui::Context,
}

// The tray state is initialized once when the app starts. The window procedure
// runs on the tray thread and uses this to wake egui for menu actions.
static TRAY_STATE: OnceLock<TrayState> = OnceLock::new();

pub struct TrayIcon {
    hwnd: HWND,
    thread: Option<JoinHandle<()>>,
}

impl TrayIcon {
    pub fn start(
        ctx: eframe::egui::Context,
        sender: Sender<TrayAction>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if TRAY_STATE.set(TrayState { sender, ctx }).is_err() {
            return Err("tray icon was already initialized".into());
        }

        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<isize, String>>(1);
        let thread = thread::Builder::new()
            .name("wallpaper-tray".into())
            .spawn(move || {
                let result = run_tray_thread(&ready_tx);
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(error));
                }
            })?;

        let hwnd = match ready_rx.recv() {
            Ok(Ok(raw)) => HWND(raw as *mut core::ffi::c_void),
            Ok(Err(error)) => return Err(error.into()),
            Err(error) => return Err(error.into()),
        };

        Ok(Self {
            hwnd,
            thread: Some(thread),
        })
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_tray_thread(ready: &mpsc::SyncSender<Result<isize, String>>) -> Result<(), String> {
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
        let class_name = w!("RustWallpaperTrayWindow");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(tray_window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&window_class);

        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            w!("Rust Wallpaper Tray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            None,
        )
        .map_err(|error| error.to_string())?;

        if let Err(error) = install_icon(hwnd) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }

        ready
            .send(Ok(hwnd.0 as isize))
            .map_err(|error| error.to_string())?;

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        remove_icon(hwnd);
        Ok(())
    }
}

unsafe extern "system" fn tray_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == taskbar_created_message() {
        // Explorer restarted; its notification area no longer has our icon.
        let _ = install_icon(hwnd);
        return LRESULT(0);
    }

    match message {
        TRAY_CALLBACK => {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => notify(TrayAction::Show),
                WM_RBUTTONUP => show_context_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            remove_icon(hwnd);
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let _ = AppendMenuW(menu, MF_STRING, MENU_NEXT, w!("⏭ Next Wallpaper"));
    let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE_PAUSE, w!("⏯ Pause / Resume"));
    let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE_MUTE, w!("🔇 Mute / Unmute"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Open Rust Wallpaper"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit Rust Wallpaper"));

    let mut point = POINT::default();
    let _ = GetCursorPos(&mut point);
    let _ = SetForegroundWindow(hwnd);
    let selected = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        None,
    )
    .0 as usize;
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match selected {
        MENU_SHOW => notify(TrayAction::Show),
        MENU_QUIT => notify(TrayAction::Quit),
        MENU_NEXT => notify(TrayAction::NextWallpaper),
        MENU_TOGGLE_PAUSE => notify(TrayAction::TogglePause),
        MENU_TOGGLE_MUTE => notify(TrayAction::ToggleMute),
        _ => {}
    }
}

static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

fn taskbar_created_message() -> u32 {
    *TASKBAR_CREATED.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

unsafe fn remove_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &data);
}

unsafe fn install_icon(hwnd: HWND) -> Result<(), String> {
    let mut icon_data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK,
        ..Default::default()
    };

    if let Some(icon) = load_app_icon() {
        icon_data.hIcon = icon;
    } else {
        icon_data.uFlags = NIF_MESSAGE | NIF_TIP;
    }
    let tip = "Rust Wallpaper - right-click to quit";
    let tip_len = tip.encode_utf16().count().min(icon_data.szTip.len() - 1);
    for (destination, source) in icon_data
        .szTip
        .iter_mut()
        .zip(tip.encode_utf16().take(tip_len))
    {
        *destination = source;
    }

    if Shell_NotifyIconW(NIM_ADD, &icon_data).0 == 0 {
        Err("Windows rejected the notification-area icon".into())
    } else {
        Ok(())
    }
}

fn notify(action: TrayAction) {
    if let Some(state) = TRAY_STATE.get() {
        let _ = state.sender.send(action);
        state.ctx.request_repaint();
    }
}

unsafe fn load_app_icon() -> Option<HICON> {
    let path = app_icon_path()?;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let loaded = LoadImageW(
        None,
        PCWSTR(wide.as_ptr()),
        IMAGE_ICON,
        0,
        0,
        LR_LOADFROMFILE | LR_DEFAULTSIZE,
    );
    if let Ok(handle) = loaded {
        return Some(HICON(handle.0));
    }
    LoadIconW(None, IDI_APPLICATION).ok()
}

fn app_icon_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("assets").join("icon.ico"));
            candidates.push(parent.join("icon.ico"));
        }
    }
    candidates.push(PathBuf::from("assets").join("icon.ico"));
    candidates.into_iter().find(|path| path.is_file())
}

use std::os::windows::ffi::OsStrExt;

// Re-export the receiver type so the UI can keep its field type explicit.
pub type TrayReceiver = Receiver<TrayAction>;
