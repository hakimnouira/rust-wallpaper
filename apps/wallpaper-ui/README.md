# Rust Wallpaper UI

The native Windows control panel for Rust Wallpaper.

## Responsibilities

- Browse the daemon-owned wallpaper library.
- Import and remove supported video files through IPC.
- Select a monitor and assign a wallpaper.
- Pause or resume individual displays or all displays.
- Display monitor geometry, player state and daemon connection health.
- Visualize monitor topology and select a target display by clicking its screen.
- Adjust volume and mute independently for each monitor.

The UI is a client only. It does not create Win32 wallpaper windows, start MPV, or access the SQLite database directly. All operations go through `wallpaper-protocol` and the daemon.

## Build

On Windows:

```powershell
cargo run -p wallpaper-ui
```

Start `wallpaper-daemon` first. The UI reconnects automatically when the daemon is unavailable or restarts.

## Current limitations

The first UI milestone uses a path field for importing videos; a native file picker, thumbnail gallery, tray integration, settings persistence, WebView2 scenes and installer are still future work.
