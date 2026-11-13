//! Native Win32 wallpaper window implementation.
//!
//! Creates borderless child windows attached directly into the Windows desktop host
//! (`WorkerW` or `Progman`), properly sized to monitor geometry, rendered with a clean
//! solid background and monitor diagnostics, without stealing keyboard focus or
//! interfering with taskbar / desktop icon interaction.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WindowError {
    #[error("wallpaper window creation is only supported on Windows")]
    UnsupportedPlatform,
    #[error("failed to register Win32 window class: {0}")]
    ClassRegistrationFailed(String),
    #[error("failed to create Win32 window: {0}")]
    WindowCreationFailed(String),
    #[error("Windows API error: {0}")]
    ApiError(String),
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracing::{debug, info};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
        SetBkMode, SetTextColor, DT_CENTER, DT_NOPREFIX, DT_VCENTER, DT_WORDBREAK, HBRUSH, HDC,
        PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW,
        RegisterClassExW, SetWindowLongPtrW, SetWindowPos, GWLP_USERDATA, HWND_BOTTOM,
        MA_NOACTIVATE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_DESTROY, WM_ERASEBKGND,
        WM_MOUSEACTIVATE, WM_PAINT, WM_SETFOCUS, WNDCLASSEXW, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
    };

    const WINDOW_CLASS_NAME: PCWSTR = w!("RustWallpaperWindow");
    static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

    struct WindowCustomData {
        device_name: String,
        width: u32,
        height: u32,
        is_primary: bool,
        brush: HBRUSH,
    }

    unsafe extern "system" fn wallpaper_wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_ERASEBKGND => {
                // Prevent background erase flicker
                LRESULT(1)
            }
            WM_MOUSEACTIVATE => {
                // Do not activate or steal focus when clicked
                LRESULT(MA_NOACTIVATE as isize)
            }
            WM_SETFOCUS => {
                // Do not retain focus
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc: HDC = BeginPaint(hwnd, &mut ps);

                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowCustomData;
                if !ptr.is_null() {
                    let data = &*ptr;
                    let mut client_rect = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client_rect);

                    // Fill background
                    FillRect(hdc, &client_rect, data.brush);

                    // Render monitor identification overlay
                    SetBkMode(hdc, TRANSPARENT);
                    SetTextColor(hdc, COLORREF(0x00E2E8F0)); // Light slate

                    let primary_tag = if data.is_primary { " (Primary)" } else { "" };
                    let banner = format!(
                        "Rust Wallpaper Engine\nMonitor: {}{}\nResolution: {} x {}\nAttached behind desktop icons",
                        data.device_name, primary_tag, data.width, data.height
                    );

                    let mut wide_text: Vec<u16> =
                        banner.encode_utf16().chain(std::iter::once(0)).collect();
                    let text_len = wide_text.len().saturating_sub(1);
                    let mut text_rect = client_rect;
                    DrawTextW(
                        hdc,
                        &mut wide_text[..text_len],
                        &mut text_rect,
                        DT_CENTER | DT_VCENTER | DT_WORDBREAK | DT_NOPREFIX,
                    );
                }

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut WindowCustomData;
                if !ptr.is_null() {
                    let _ = Box::from_raw(ptr);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    fn ensure_window_class() -> Result<(), WindowError> {
        if CLASS_REGISTERED.load(Ordering::Acquire) {
            return Ok(());
        }

        unsafe {
            let hinstance = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| WindowError::ClassRegistrationFailed(e.to_string()))?;

            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wallpaper_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance.into(),
                hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
                hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: WINDOW_CLASS_NAME,
                hIconSm: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
            };

            let atom = RegisterClassExW(&wc);
            if atom == 0 {
                let err = windows::core::Error::from_win32();
                // If the class already exists, that's fine
                if err.code().0 != (0x80070582u32 as i32) && err.code().0 != 1410 {
                    return Err(WindowError::ClassRegistrationFailed(err.to_string()));
                }
            }
        }

        CLASS_REGISTERED.store(true, Ordering::Release);
        Ok(())
    }

    /// An active native desktop wallpaper window.
    pub struct WallpaperWindow {
        pub hwnd: HWND,
        pub host_hwnd: HWND,
        pub device_name: String,
        pub client_x: i32,
        pub client_y: i32,
        pub width: u32,
        pub height: u32,
        pub is_primary: bool,
        brush: HBRUSH,
    }

    impl WallpaperWindow {
        /// Creates and attaches a borderless wallpaper window directly into `host_hwnd`.
        pub fn create(
            host_hwnd: HWND,
            device_name: String,
            client_x: i32,
            client_y: i32,
            width: u32,
            height: u32,
            is_primary: bool,
            bg_color: COLORREF,
        ) -> Result<Self, WindowError> {
            ensure_window_class()?;

            let brush = unsafe { CreateSolidBrush(bg_color) };

            let custom_data = Box::new(WindowCustomData {
                device_name: device_name.clone(),
                width,
                height,
                is_primary,
                brush,
            });

            let hinstance = unsafe {
                GetModuleHandleW(PCWSTR::null())
                    .map_err(|e| WindowError::WindowCreationFailed(e.to_string()))?
            };

            // Create a borderless popup window first, then attach via SetParent.
            // Direct WS_CHILD creation into cross-process WorkerW prevents MPV's Win32/DirectX
            // windowing subsystem from attaching its surface. Reparenting via SetParent enables
            // full child embedding behind desktop icons while allowing MPV to render smoothly.
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    WINDOW_CLASS_NAME,
                    w!("Rust Wallpaper"),
                    WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                    client_x,
                    client_y,
                    width as i32,
                    height as i32,
                    HWND::default(),
                    windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
                    hinstance,
                    None,
                )
                .map_err(|e| WindowError::WindowCreationFailed(e.to_string()))?
            };

            let data_ptr = Box::into_raw(custom_data);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, data_ptr as isize);
                // Note: Keep as a clean top-level popup initially so MPV's process can attach its child window
                // via CreateWindowEx without violating Win32 cross-process child restrictions.
                // It is then attached to WorkerW via `attach_to_host()` after MPV attaches.
                let _ = SetWindowPos(
                    hwnd,
                    HWND_BOTTOM,
                    client_x,
                    client_y,
                    width as i32,
                    height as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }

            info!(
                hwnd = format_args!("0x{:X}", hwnd.0 as isize),
                device = %device_name,
                x = client_x,
                y = client_y,
                width = width,
                height = height,
                primary = is_primary,
                "Wallpaper window attached to desktop host"
            );

            Ok(WallpaperWindow {
                hwnd,
                host_hwnd,
                device_name,
                client_x,
                client_y,
                width,
                height,
                is_primary,
                brush,
            })
        }

        /// Attaches this window to the desktop host (behind icons in WorkerW).
        pub fn attach_to_host(&self) {
            unsafe {
                let _ =
                    windows::Win32::UI::WindowsAndMessaging::SetParent(self.hwnd, self.host_hwnd);
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_BOTTOM,
                    self.client_x,
                    self.client_y,
                    self.width as i32,
                    self.height as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
                );
                let _ = InvalidateRect(self.hwnd, None, true);
            }
        }

        /// Updates the window geometry (e.g. following display resolution changes).
        pub fn update_geometry(&mut self, client_x: i32, client_y: i32, width: u32, height: u32) {
            self.client_x = client_x;
            self.client_y = client_y;
            self.width = width;
            self.height = height;

            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_BOTTOM,
                    client_x,
                    client_y,
                    width as i32,
                    height as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                let _ = InvalidateRect(self.hwnd, None, true);
            }
        }
    }

    impl Drop for WallpaperWindow {
        fn drop(&mut self) {
            debug!(
                hwnd = format_args!("0x{:X}", self.hwnd.0 as isize),
                device = %self.device_name,
                "Destroying wallpaper window"
            );
            unsafe {
                let _ = DestroyWindow(self.hwnd);
                let _ = DeleteObject(self.brush);
                // Invalidate host region so desktop redraws cleanly
                let _ = InvalidateRect(self.host_hwnd, None, true);
            }
        }
    }
}

#[cfg(windows)]
pub use platform::WallpaperWindow;

#[cfg(not(windows))]
pub struct WallpaperWindow {
    pub hwnd: isize,
    pub device_name: String,
    pub client_x: i32,
    pub client_y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[cfg(not(windows))]
impl WallpaperWindow {
    pub fn update_geometry(&mut self, client_x: i32, client_y: i32, width: u32, height: u32) {
        self.client_x = client_x;
        self.client_y = client_y;
        self.width = width;
        self.height = height;
    }
}
