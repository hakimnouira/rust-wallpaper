# Rust Wallpaper

A Windows-first live wallpaper engine written in Rust, inspired by the architecture and user-facing capabilities of [Lively Wallpaper](https://github.com/rocksdanister/lively).

This is an independent implementation. Lively is used as a behavioral and architectural reference; its source code is not copied.

## Project status

**Windows release candidate.** Core playback, daemon, IPC, library UI, installer, and packaging workflows are implemented. A clean-machine interactive Windows install and runtime validation is still required before calling this a final/stable release.

The current workspace contains a Windows wallpaper daemon, a multi-monitor runtime supervisor, MPV-backed video playback, monitor assignment persistence, fullscreen-aware playback policies, system-event handling, player recovery, a named-pipe IPC service, and an initial egui desktop control panel.

This is an early Windows video-wallpaper application, not a feature-complete replacement for mature wallpaper products. It can browse/import/remove library entries, assign videos, control monitor playback, and change performance profiles. Scene/WebView2 rendering and a polished updater are not included. Real-machine Windows validation remains required.

## Current capabilities

- Native Windows desktop wallpaper integration.
- MPV-backed video wallpaper playback.
- Multi-monitor discovery and per-monitor assignment lifecycle.
- Persistent monitor assignments.
- Fullscreen-aware pause policies and manual pause state.
- Single-instance enforcement for both the control panel and the daemon; launching the panel again restores and focuses the existing window instead of opening a duplicate.
- Persistent daily logs under `%LOCALAPPDATA%\RustWallpaper\logs`; the Settings page can open the logs and application-data folders.
- Player process health reporting and automatic recovery.
- Runtime/core tests and Windows GNU cross-target compilation checks.
- Redesigned control panel with an Overview dashboard, dedicated Appearance page, themed cards, and clearer navigation, alongside library search/import, monitor topology visualization, per-monitor playback controls, saved performance profiles, and persistent playback automation settings (fullscreen pause mode, lock/display/suspend policies, FPS limit).
- Wallpaper Engine-style playback settings: configurable framerate limit, RAM budget and scaling filter applied to active players, plus explicit fullscreen pause behavior that survives profile changes and restarts.
- Windows notification-area icon: closing the window keeps the app running; the tray menu can show the window or fully quit the UI and daemon. Windows autostart launches the UI hidden in the notification area.

## Architecture

```text
apps/
  wallpaper-daemon/   Runtime coordinator, Windows integration and IPC server
  wallpaper-ui/       Native egui control panel and IPC client
  video-player/       Player application
crates/
  wallpaper-core/     Domain types, policies, lifecycle and supervision
  wallpaper-windows/  Win32 integration, monitor discovery and MPV management
  wallpaper-protocol/ Versioned daemon/client IPC contract
docs/
  architecture.md
```

The daemon owns wallpaper lifecycle, Win32 desktop integration and MPV processes. The desktop UI is a separate IPC client: it never manipulates wallpaper windows or starts players directly.

## Development

Requirements:

- Rust stable
- Windows 10/11 for native desktop integration
- MPV runtime bundled by `scripts/build-production.ps1` from `%USERPROFILE%\.mpv`
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
cargo run -p wallpaper-ui
```

The daemon stores its SQLite library and monitor assignments under `%LOCALAPPDATA%\RustWallpaper\library.db`. In a packaged install, demo wallpaper assets are resolved beside the installed binaries, independent of the process working directory.

## Building a Windows release candidate

On Windows, install the Rust stable toolchain, Python 3, OpenSSL, and a trusted x64 MPV distribution under `%USERPROFILE%\.mpv`. The package builder fails if required release inputs are missing:

```powershell
.\scripts\build-production.ps1
```

Distribute `dist\RustWallpaper-0.1.0-windows-x64.zip` together with its `.sha256` checksum. The ZIP contains the setup program and the companion binaries/assets it needs; users should extract the ZIP and run `RustWallpaper-Setup.exe`, not download the setup EXE alone. MPV and FFmpeg are third-party components: check their licenses and redistribution terms and include the applicable notices before public distribution.

This repository currently has no selected project license, so the project source itself is not yet cleared for public redistribution. A Windows code-signing certificate has also not been configured; unsigned downloads may trigger SmartScreen warnings. Before calling the release stable, perform a clean Windows install/playback/startup/uninstall test using `docs/windows-smoke-tests.md`.

## Current priorities

1. Validate the installer and runtime on a clean Windows machine: install, playback, autostart, uninstall, multi-monitor, sleep/wake, lock/unlock, Explorer restart, MPV crash recovery and fullscreen games.
2. Select a project license and audit third-party redistribution notices.
3. Configure release signing and publish reproducible checksums.
4. Continue polishing library management, tray integration and updater behavior.
5. Evaluate WebView2 and scene wallpaper support after the video engine and application are stable.

## License

License selection is pending. Until a license is added, the repository should be treated as all-rights-reserved.
