# Windows smoke-test checklist

Run these checks on a Windows machine after a daemon/UI build. The cross-platform unit tests cover policy decisions, but these checks exercise Explorer, Win32 windows, named pipes, MPV, and actual power/display events.

## Single instance and diagnostics

1. Launch the UI twice; confirm the second launch exits and the existing window is shown/focused instead of opening another control panel.
2. Start `wallpaper-daemon.exe` while the UI (with its running daemon) is open; confirm the standalone daemon exits immediately and playback is unaffected.
3. Confirm daily `ui.*.log` and `daemon.*.log` files exist under `%LOCALAPPDATA%\RustWallpaper\logs`, then use Settings > Open logs and Open data folder.
4. While the window is hidden in the notification area, confirm CPU use settles and the tray icon still restores the window within a moment.

## Notification-area lifecycle

1. Launch the UI and confirm the Rust Wallpaper icon appears in the Windows notification area (including the hidden-icons overflow on Windows 11).
2. Close the main window with the X button; confirm the window hides but the tray icon remains and playback continues.
3. Left-click or double-click the tray icon; confirm the window is restored and focused.
4. Right-click the tray icon and choose Quit; confirm the UI closes, the tray icon disappears, and the daemon/MPV processes stop.
5. Restart Windows Explorer while the app is running; confirm the tray icon is re-created and its menu still works.
6. Enable Windows startup, reboot or sign out/in, and confirm the app starts hidden with a working tray icon; then disable startup and confirm the Run entry is removed.

## IPC security and reconnect

1. Start the daemon and UI under the same Windows account/session; confirm the UI connects and Ping/status requests work.
2. Restart the daemon while the UI remains open; confirm the client reconnects and status refresh resumes.
3. From a different Windows account or session, attempt to open `\\.\pipe\rust-wallpaper-ipc`; confirm connection is denied by the pipe ACL or the client is rejected before any request is served.
4. Confirm a same-user client in the current session still connects. Confirm remote named-pipe clients are rejected.

## MPV recovery and responsiveness

1. Apply a video wallpaper to two monitors, pause/resume, and adjust mute/volume. Confirm one unresponsive MPV does not freeze controls or health checks for the other monitor.
2. Terminate one MPV process externally. Confirm the daemon logs recovery attempts and restarts playback with the existing assignment.
3. Make an MPV IPC endpoint unresponsive while the process remains alive. Confirm health probes are asynchronous, recovery is bounded/backed off, and the daemon continues servicing IPC requests.
4. Stop/reassign a wallpaper while one player is slow to exit; confirm other monitors remain manageable and the daemon eventually cleans up the process.

## Explorer, sleep, and displays

1. Restart Windows Explorer while wallpapers are active; confirm wallpaper windows reattach and playback/suspension state is preserved.
2. Lock/unlock the session and sleep/resume the machine; confirm playback remains suspended while appropriate and resumes afterward.
3. Add, remove, reconnect, and renumber a monitor; change resolution/orientation; confirm unaffected monitors keep their assignments and the topology is refreshed.
4. Repeat with a fullscreen game active on one display and a user-paused wallpaper on another; verify suspension reasons remain independent.

## Online download UX

1. Start a large video download from the browse page and from a direct URL. Confirm byte progress appears, Cancel stops the transfer, no partial destination is imported, and the failure/cancellation message is visible.
2. Interrupt network access during a download and confirm the UI reports the error and clears any pending auto-apply action.
