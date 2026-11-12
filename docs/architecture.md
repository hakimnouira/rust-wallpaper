# Architecture notes

## Design principles

1. Keep Windows APIs behind `wallpaper-windows`; domain types must compile on other platforms.
2. Keep the control UI separate from wallpaper renderer processes.
3. Use a versioned, serializable IPC contract.
4. Treat each player as a supervised child process with explicit start, pause, resume, and stop behavior.
5. Release GPU, decoder, and browser resources when players stop.
6. Do not assume a single desktop-host window or monitor topology.

## Initial process model

- **wallpaper-daemon**: owns application lifecycle, display enumeration, player supervision, and desktop integration.
- **desktop-ui** (future): library, settings, import, and user commands.
- **video-player** (future): video decoding and presentation.
- **web-player** (future): WebView2 content in a separate process.
- **watchdog** (future): restart policy and health reporting.

## Windows desktop integration research

The first Windows integration step is monitor enumeration. The next step is a small native test window and a diagnostic tool that inspects Progman/WorkerW relationships. Desktop attachment should be implemented only after validating behavior across Windows 10/11, Explorer restarts, taskbar visibility, virtual desktops, and multiple monitors.

Avoid treating undocumented WorkerW discovery tricks as stable contracts. Isolate any compatibility logic, add diagnostics, and provide graceful fallback behavior.

## IPC

The protocol crate defines versioned request and response envelopes. The transport will be added separately; on Windows, named pipes are the likely initial transport. Validate protocol versions and message sizes at the transport boundary.

## Video playback

The planned first player is MPV/libmpv. Keep playback controls behind a player adapter so a future renderer can be substituted without changing the daemon's domain model.

## Performance policy

A policy service should evaluate visibility, fullscreen activity, power state, lock state, and user preferences. It should emit player lifecycle commands rather than embedding policy in each renderer.
