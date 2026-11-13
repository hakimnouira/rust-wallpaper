//! Windows desktop host discovery and WorkerW orchestration.
//!
//! # Architecture & Windows Internals
//!
//! Windows Explorer uses a composite window architecture to manage desktop icons
//! and wallpaper drawing:
//!
//! 1. **Program Manager (`Progman`)**: Top-level shell window created by `explorer.exe`.
//! 2. **`SHELLDLL_DefView`**: The desktop view container holding the desktop icon listview
//!    (`SysListView32`).
//! 3. **`0x052C` (WM_SPAWN_WORKER)**: An undocumented message sent to `Progman` that instructs
//!    Explorer to separate desktop icon rendering from background rendering by spawning
//!    one or more `WorkerW` windows.
//! 4. **`WorkerW` Layering**:
//!    - One `WorkerW` becomes the parent of `SHELLDLL_DefView` (holding desktop icons).
//!    - A sibling `WorkerW` is placed immediately behind it in the Z-order.
//!    - By reparenting a custom window into this background `WorkerW`, custom content renders
//!      underneath desktop icons while staying above the static Windows desktop wallpaper.
//!
//! Multiple `WorkerW` windows can exist simultaneously on Windows (for task switching,
//! transitions, or multiple shells). We must never assume the first `WorkerW` is the correct one;
//! instead, we locate the window containing `SHELLDLL_DefView` and find its sibling `WorkerW`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesktopHostError {
    #[error("desktop host discovery is only supported on Windows")]
    UnsupportedPlatform,
    #[error("failed to find Explorer Program Manager (Progman) window")]
    ProgmanNotFound,
    #[error("failed to send spawn message 0x052C to Progman: {0}")]
    MessageFailed(String),
    #[error("failed to locate desktop shell view (SHELLDLL_DefView)")]
    ShellViewNotFound,
    #[error("failed to locate valid WorkerW or Progman wallpaper host window")]
    HostNotFound,
    #[error("Windows API call failed: {0}")]
    ApiError(String),
}

/// The kind of desktop host window discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopHostKind {
    /// A background WorkerW window positioned behind desktop icons (standard Windows 10/11).
    WorkerW,
    /// Direct Progman window (fallback mode if WorkerW cannot be spawned).
    Progman,
}

/// Diagnostic details for an enumerated WorkerW window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerWInfo {
    pub hwnd: isize,
    pub has_shelldll: bool,
    pub rect: (i32, i32, i32, i32),
}

/// Comprehensive diagnostics captured during desktop host discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopHostDiagnostics {
    pub progman_hwnd: isize,
    pub shell_container_hwnd: Option<isize>,
    pub shell_view_hwnd: Option<isize>,
    pub workerw_windows: Vec<WorkerWInfo>,
    pub selected_host_hwnd: isize,
    pub host_kind: DesktopHostKind,
    pub host_rect: (i32, i32, i32, i32),
}

#[cfg(windows)]
mod platform {
    use super::*;
    use tracing::{debug, info, warn};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::System::StationsAndDesktops::{
        GetThreadDesktop, OpenDesktopW, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
        DESKTOP_CONTROL_FLAGS,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowRect, IsWindow,
        SendMessageTimeoutW, SMTO_NORMAL,
    };

    /// Represents an active Windows desktop host suitable for parenting wallpaper windows.
    #[derive(Debug, Clone)]
    pub struct DesktopHost {
        pub hwnd: HWND,
        pub kind: DesktopHostKind,
        pub rect: RECT,
        pub diagnostics: DesktopHostDiagnostics,
    }

    impl DesktopHost {
        /// Converts screen coordinates to client coordinates relative to the desktop host window.
        #[inline]
        pub fn screen_to_client(&self, screen_x: i32, screen_y: i32) -> (i32, i32) {
            (screen_x - self.rect.left, screen_y - self.rect.top)
        }

        /// Checks whether the desktop host HWND is still valid and alive in Windows.
        #[inline]
        pub fn is_valid(&self) -> bool {
            unsafe { IsWindow(self.hwnd).as_bool() }
        }

        /// Refreshes and returns the current bounding rectangle of the desktop host.
        pub fn update_rect(&mut self) -> Result<RECT, DesktopHostError> {
            let mut rect = RECT::default();
            unsafe {
                GetWindowRect(self.hwnd, &mut rect)
                    .map_err(|e| DesktopHostError::ApiError(e.to_string()))?;
            }
            self.rect = rect;
            Ok(rect)
        }
    }

    /// Ensures the calling thread is attached to the interactive "default" desktop.
    fn ensure_interactive_desktop() {
        unsafe {
            // Check if we can open the interactive "default" desktop.
            // When launched from background services, IDE runners, or sandbox environments,
            // the thread may be associated with a non-interactive desktop.
            let desktop_rights = DESKTOP_ACCESS_FLAGS(0x01FF); // DESKTOP_ALL
            if let Ok(hdesktop) = OpenDesktopW(
                w!("default"),
                DESKTOP_CONTROL_FLAGS(0),
                false,
                desktop_rights.0,
            ) {
                let current_desktop = GetThreadDesktop(GetCurrentThreadId());
                if current_desktop != Ok(hdesktop) {
                    let _ = SetThreadDesktop(hdesktop);
                }
            }
        }
    }

    /// Discovers the active Windows desktop host and returns structured diagnostics.
    pub fn ensure_desktop_host() -> Result<DesktopHost, DesktopHostError> {
        ensure_interactive_desktop();

        // 1. Find the Program Manager (Progman) window
        let progman = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }
            .map_err(|_| DesktopHostError::ProgmanNotFound)?;

        if progman.0 == std::ptr::null_mut() || !unsafe { IsWindow(progman).as_bool() } {
            return Err(DesktopHostError::ProgmanNotFound);
        }

        let progman_raw = progman.0 as isize;
        debug!(
            progman = format_args!("0x{:X}", progman_raw),
            "Located Progman window"
        );

        // 2. Send 0x052C to Progman to instruct Explorer to spawn the WorkerW background layer
        let mut send_result = 0usize;
        unsafe {
            let _ = SendMessageTimeoutW(
                progman,
                0x052C,
                WPARAM(0x0000000D),
                LPARAM(0x00000001),
                SMTO_NORMAL,
                1000,
                Some(&mut send_result),
            );
        }

        // 3. Enumerate top-level windows to discover WorkerW instances and SHELLDLL_DefView
        struct DiscoveryContext {
            shell_container: Option<HWND>,
            shell_view: Option<HWND>,
            workerw_list: Vec<WorkerWInfo>,
        }

        let mut context = DiscoveryContext {
            shell_container: None,
            shell_view: None,
            workerw_list: Vec::new(),
        };

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut DiscoveryContext);

            let mut class_buf = [0u16; 256];
            let len = GetClassNameW(hwnd, &mut class_buf);
            if len > 0 {
                let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);

                // Check for SHELLDLL_DefView inside this window
                let shell_view = FindWindowExW(
                    hwnd,
                    HWND::default(),
                    w!("SHELLDLL_DefView"),
                    PCWSTR::null(),
                )
                .ok();

                if let Some(view) = shell_view {
                    if view.0 != std::ptr::null_mut() {
                        ctx.shell_container = Some(hwnd);
                        ctx.shell_view = Some(view);
                    }
                }

                if class_name == "WorkerW" {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut r);
                    ctx.workerw_list.push(WorkerWInfo {
                        hwnd: hwnd.0 as isize,
                        has_shelldll: shell_view
                            .map(|v| v.0 != std::ptr::null_mut())
                            .unwrap_or(false),
                        rect: (r.left, r.top, r.right - r.left, r.bottom - r.top),
                    });
                }
            }

            BOOL(1)
        }

        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut context as *mut _ as isize));
        }

        // 4. Identify the wallpaper host window
        let mut selected_host: Option<(HWND, DesktopHostKind)> = None;

        if let Some(shell_container) = context.shell_container {
            debug!(
                shell_container = format_args!("0x{:X}", shell_container.0 as isize),
                shell_view = format_args!(
                    "0x{:X}",
                    context.shell_view.map(|v| v.0 as isize).unwrap_or(0)
                ),
                "Found SHELLDLL_DefView desktop container"
            );

            // In standard Windows 10/11, the WorkerW directly behind the SHELLDLL_DefView container
            // is the dedicated wallpaper rendering host.
            let workerw_behind = unsafe {
                FindWindowExW(
                    HWND::default(),
                    shell_container,
                    w!("WorkerW"),
                    PCWSTR::null(),
                )
            }
            .ok();

            if let Some(workerw) = workerw_behind {
                if workerw.0 != std::ptr::null_mut() && unsafe { IsWindow(workerw).as_bool() } {
                    selected_host = Some((workerw, DesktopHostKind::WorkerW));
                }
            }
        }

        // Fallback: If no sibling WorkerW was found, check if a WorkerW without SHELLDLL_DefView exists
        if selected_host.is_none() {
            for win in &context.workerw_list {
                if !win.has_shelldll {
                    let candidate = HWND(win.hwnd as *mut core::ffi::c_void);
                    if unsafe { IsWindow(candidate).as_bool() } {
                        debug!(
                            fallback_workerw = format_args!("0x{:X}", win.hwnd),
                            "Selecting fallback WorkerW without SHELLDLL_DefView"
                        );
                        selected_host = Some((candidate, DesktopHostKind::WorkerW));
                        break;
                    }
                }
            }
        }

        // Fallback: If still no WorkerW, fallback to Progman itself
        if selected_host.is_none() {
            warn!("No suitable WorkerW window found; falling back to direct Progman host");
            selected_host = Some((progman, DesktopHostKind::Progman));
        }

        let (host_hwnd, host_kind) = selected_host.ok_or(DesktopHostError::HostNotFound)?;

        let mut host_rect = RECT::default();
        unsafe {
            GetWindowRect(host_hwnd, &mut host_rect)
                .map_err(|e| DesktopHostError::ApiError(e.to_string()))?;
        }

        let diagnostics = DesktopHostDiagnostics {
            progman_hwnd: progman_raw,
            shell_container_hwnd: context.shell_container.map(|h| h.0 as isize),
            shell_view_hwnd: context.shell_view.map(|h| h.0 as isize),
            workerw_windows: context.workerw_list,
            selected_host_hwnd: host_hwnd.0 as isize,
            host_kind,
            host_rect: (
                host_rect.left,
                host_rect.top,
                host_rect.right - host_rect.left,
                host_rect.bottom - host_rect.top,
            ),
        };

        info!(
            host_hwnd = format_args!("0x{:X}", diagnostics.selected_host_hwnd),
            kind = ?diagnostics.host_kind,
            rect = ?diagnostics.host_rect,
            workerw_count = diagnostics.workerw_windows.len(),
            "Desktop host selected successfully"
        );

        Ok(DesktopHost {
            hwnd: host_hwnd,
            kind: host_kind,
            rect: host_rect,
            diagnostics,
        })
    }
}

#[cfg(windows)]
pub use platform::{ensure_desktop_host, DesktopHost};

#[cfg(not(windows))]
#[derive(Debug, Clone)]
pub struct DesktopHost {
    pub hwnd: isize,
    pub kind: DesktopHostKind,
    pub rect: (i32, i32, i32, i32),
    pub diagnostics: DesktopHostDiagnostics,
}

#[cfg(not(windows))]
impl DesktopHost {
    pub fn screen_to_client(&self, screen_x: i32, screen_y: i32) -> (i32, i32) {
        (screen_x - self.rect.0, screen_y - self.rect.1)
    }

    pub fn is_valid(&self) -> bool {
        false
    }
}

#[cfg(not(windows))]
pub fn ensure_desktop_host() -> Result<DesktopHost, DesktopHostError> {
    Err(DesktopHostError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_to_client_handles_primary_and_offsets() {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::RECT;
            let host = DesktopHost {
                hwnd: windows::Win32::Foundation::HWND(0x1234 as *mut core::ffi::c_void),
                kind: DesktopHostKind::WorkerW,
                rect: RECT {
                    left: -1920,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                diagnostics: DesktopHostDiagnostics {
                    progman_hwnd: 0x1000,
                    shell_container_hwnd: Some(0x2000),
                    shell_view_hwnd: Some(0x3000),
                    workerw_windows: vec![],
                    selected_host_hwnd: 0x1234,
                    host_kind: DesktopHostKind::WorkerW,
                    host_rect: (-1920, 0, 3840, 1080),
                },
            };

            // Primary monitor at (0, 0) maps to client coordinate (1920, 0)
            assert_eq!(host.screen_to_client(0, 0), (1920, 0));
            // Secondary monitor at (-1920, 0) maps to client coordinate (0, 0)
            assert_eq!(host.screen_to_client(-1920, 0), (0, 0));
            // Secondary monitor at (1920, 0) maps to client coordinate (3840, 0)
            assert_eq!(host.screen_to_client(1920, 0), (3840, 0));
        }

        #[cfg(not(windows))]
        {
            let host_rect = (-1920, 0, 3840, 1080);
            let host = DesktopHost {
                hwnd: 0x1234,
                kind: DesktopHostKind::WorkerW,
                rect: host_rect,
                diagnostics: DesktopHostDiagnostics {
                    progman_hwnd: 0x1000,
                    shell_container_hwnd: Some(0x2000),
                    shell_view_hwnd: Some(0x3000),
                    workerw_windows: vec![],
                    selected_host_hwnd: 0x1234,
                    host_kind: DesktopHostKind::WorkerW,
                    host_rect,
                },
            };

            assert_eq!(host.screen_to_client(0, 0), (1920, 0));
            assert_eq!(host.screen_to_client(-1920, 0), (0, 0));
        }
    }

    #[test]
    fn diagnostics_captures_correct_fields() {
        let diag = DesktopHostDiagnostics {
            progman_hwnd: 0x10186,
            shell_container_hwnd: Some(0x104AA),
            shell_view_hwnd: Some(0x1018E),
            workerw_windows: vec![
                WorkerWInfo {
                    hwnd: 0x104AA,
                    has_shelldll: true,
                    rect: (0, 0, 3328, 1152),
                },
                WorkerWInfo {
                    hwnd: 0x104AC,
                    has_shelldll: false,
                    rect: (0, 0, 3328, 1152),
                },
            ],
            selected_host_hwnd: 0x104AC,
            host_kind: DesktopHostKind::WorkerW,
            host_rect: (0, 0, 3328, 1152),
        };

        assert_eq!(diag.selected_host_hwnd, 0x104AC);
        assert_eq!(diag.host_kind, DesktopHostKind::WorkerW);
        assert_eq!(diag.workerw_windows.len(), 2);
        assert!(diag.workerw_windows[0].has_shelldll);
        assert!(!diag.workerw_windows[1].has_shelldll);
    }
}
