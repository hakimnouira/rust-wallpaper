//! Single-instance enforcement.
//!
//! A named mutex is owned for the lifetime of the process. Windows releases it
//! automatically if the process exits or crashes, so no stale lock files are
//! left behind. UI and daemon roles use separate mutexes because the UI spawns
//! the daemon as a companion process.

use windows::core::{w, HRESULT};
use windows::Win32::Foundation::{
    CloseHandle, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, WIN32_ERROR,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

/// Window title used by the control panel; also used to find the running instance.
pub const UI_WINDOW_TITLE: &str = "Rust Wallpaper";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceRole {
    Ui,
    Daemon,
}

/// Owns a named mutex. Dropping it releases the lock, and Windows also releases
/// it if the process terminates unexpectedly.
pub struct SingleInstanceGuard {
    handle: HANDLE,
}

impl SingleInstanceGuard {
    /// Returns `Ok(None)` when another instance already owns the mutex.
    pub fn acquire(role: SingleInstanceRole) -> std::io::Result<Option<Self>> {
        let name = match role {
            SingleInstanceRole::Ui => w!("Local\\RustWallpaper-UI-SingleInstance"),
            SingleInstanceRole::Daemon => w!("Local\\RustWallpaper-Daemon-SingleInstance"),
        };
        unsafe {
            // GetLastError is not cleared on success, so reset it first: otherwise
            // a stale error from an unrelated call could look like "already running".
            SetLastError(WIN32_ERROR(0));
            match CreateMutexW(None, true, name) {
                Ok(handle) => {
                    // CreateMutexW succeeds both when it creates the object and when
                    // it opens an existing one; only GetLastError tells them apart.
                    let last_error = windows::core::Error::from_win32();
                    if last_error.code() == HRESULT::from_win32(ERROR_ALREADY_EXISTS.0) {
                        let _ = CloseHandle(handle);
                        return Ok(None);
                    }
                    Ok(Some(Self { handle }))
                }
                Err(error) => Err(std::io::Error::from_raw_os_error(error.code().0)),
            }
        }
    }

    pub fn acquire_ui() -> std::io::Result<Option<Self>> {
        Self::acquire(SingleInstanceRole::Ui)
    }

    pub fn acquire_daemon() -> std::io::Result<Option<Self>> {
        Self::acquire(SingleInstanceRole::Daemon)
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Shows and focuses the already-running control panel. Returns false if no
/// window with the expected title could be found.
pub fn focus_existing_ui_window() -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(None, w!("Rust Wallpaper")) else {
            return false;
        };
        if hwnd.0.is_null() {
            return false;
        }
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        true
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::SingleInstanceGuard;

    #[test]
    fn second_ui_instance_is_rejected_and_released_on_drop() {
        let Ok(Some(first)) = SingleInstanceGuard::acquire_ui() else {
            // A real Rust Wallpaper process owns the mutex; that is the behavior
            // under test, so do not disturb it from the test runner.
            return;
        };
        assert!(
            SingleInstanceGuard::acquire_ui().unwrap().is_none(),
            "a second UI instance must not acquire the mutex"
        );
        drop(first);
        let reacquired = SingleInstanceGuard::acquire_ui().unwrap();
        assert!(reacquired.is_some(), "dropping the guard must release it");
    }
}

#[cfg(not(windows))]
mod stub {
    use super::SingleInstanceRole;

    pub struct SingleInstanceGuard;

    impl SingleInstanceGuard {
        pub fn acquire(_: SingleInstanceRole) -> std::io::Result<Option<Self>> {
            Ok(Some(Self))
        }
        pub fn acquire_ui() -> std::io::Result<Option<Self>> {
            Ok(Some(Self))
        }
        pub fn acquire_daemon() -> std::io::Result<Option<Self>> {
            Ok(Some(Self))
        }
    }

    pub fn focus_existing_ui_window() -> bool {
        false
    }
}

#[cfg(not(windows))]
pub use stub::{focus_existing_ui_window, SingleInstanceGuard};
