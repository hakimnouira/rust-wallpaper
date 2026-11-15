# Rust Wallpaper

A Windows-first live wallpaper engine written in Rust, inspired by the architecture and user-facing capabilities of [Lively Wallpaper](https://github.com/rocksdanister/lively).

This is an independent implementation. Lively is used as a behavioral and architectural reference; its source code is not copied.

## Project status

**Runtime foundation implemented; desktop application and release packaging are still in development.**

The current workspace contains a Windows wallpaper daemon, a multi-monitor runtime supervisor, MPV-backed video playback, monitor assignment persistence, fullscreen-aware playback policies, system-event handling, and player recovery.

The project is not yet a complete end-user wallpaper application. A polished desktop UI, daemon IPC service, complete library workflows, WebView2/scene rendering, installer/updater, and real-machine Windows validation remain outstanding.

## Current capabilities

- Native Windows desktop wallpaper integration.
- MPV-backed video wallpaper playback.
- Multi-monitor discovery and per-monitor assignment lifecycle.
- Persistent monitor assignments.
- Fullscreen-aware pause policies and manual pause state.
- Session lock, display power, suspend/resume, topology and Explorer-restart notifications.
- Player process health reporting and automatic recovery.
- Runtime/core tests and Windows GNU cross-target compilation checks.

## Architecture

```text
apps/
  wallpaper-daemon/   Runtime coordinator and Windows entry point
  video-player/       Player application
crates/
  wallpaper-core/     Domain types, policies, lifecycle and supervision
  wallpaper-windows/  Win32 integration, monitor discovery and MPV management
  wallpaper-protocol/ Versioned IPC message contracts (service integration pending)
docs/
  architecture.md
```

The daemon owns wallpaper lifecycle and native desktop integration. The runtime and platform crates are the current implementation focus. The protocol crate defines communication contracts, but a complete daemon IPC service and desktop UI are not yet integrated.

## Development

Requirements:

- Rust stable
- Windows 10/11 for native desktop integration
- MPV runtime / bundled player assets as configured by the workspace
- A C/C++ build toolchain may be required by native dependencies

Useful checks:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
```

For the Windows GNU cross-target check used by CI:

```powershell
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu
```

Run the daemon on Windows:

```powershell
cargo run -p wallpaper-daemon
```

The daemon stores its SQLite library and monitor assignments under `%LOCALAPPDATA%\RustWallpaper\library.db`. The demo wallpaper assets are currently resolved relative to the working directory.

## Current priorities

1. Validate the runtime on real Windows hardware: multi-monitor, sleep/wake, lock/unlock, Explorer restart, MPV crash recovery and fullscreen games.
2. Harden runtime scheduling and diagnostics; keep potentially blocking MPV IPC off the daemon's main loop.
3. Implement the daemon IPC service and desktop UI as separate application-layer components.
4. Complete library management, settings, tray integration and installer/update workflows.
5. Evaluate WebView2 and scene wallpaper support after the video engine and application are stable.

## License

License selection is pending. Until a license is added, the repository should be treated as all-rights-reserved.
