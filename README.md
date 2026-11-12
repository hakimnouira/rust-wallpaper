# Rust Wallpaper

A Windows-first live wallpaper engine written in Rust, inspired by the architecture and user-facing capabilities of [Lively Wallpaper](https://github.com/rocksdanister/lively).

This project is an independent implementation. It uses Lively as a behavioral and architectural reference; it does not copy Lively source code.

## Project status

**Early scaffold — not yet a working wallpaper player.**

The first milestone establishes the workspace, project boundaries, and Windows integration entry point. Desktop attachment, video playback, monitor orchestration, and the UI will be implemented and tested in later milestones.

## Goals

- Native Windows desktop wallpaper integration.
- Video wallpapers with hardware decoding where supported.
- Multiple-monitor awareness.
- Explicit lifecycle management and reliable cleanup.
- Pause policies for fullscreen applications, lock screen, and battery state.
- Web wallpapers through WebView2 in an isolated player process.
- A library and settings UI, kept separate from the wallpaper runtime.

## Architecture

```text
apps/
  wallpaper-daemon/   Runtime coordinator and Windows entry point
  desktop-ui/         Future user interface
crates/
  wallpaper-core/     Domain types and orchestration contracts
  wallpaper-windows/  Windows-specific APIs and monitor discovery
  wallpaper-protocol/ IPC message types
docs/
  architecture.md
```

The daemon owns wallpaper lifecycle and native desktop integration. Player implementations will run independently and communicate with the daemon over a versioned IPC protocol. The UI will be a control surface, not the rendering host.

## Development

Requirements:

- Rust stable
- Windows 10/11 for the native integration target
- A C/C++ build toolchain may be required by future native media dependencies

Run on Windows:

```powershell
cargo check --workspace
cargo test --workspace
cargo run -p wallpaper-daemon
```

On non-Windows systems, the workspace can be checked, but the Windows integration executable reports that it is Windows-only.

## Milestones

1. Workspace and Windows platform boundary.
2. Monitor enumeration and desktop host investigation.
3. Native test window and desktop attachment prototype.
4. Video player process and playback controls.
5. Multi-monitor orchestration and performance policies.
6. Web player, library, settings UI, and packaging.

## License

License selection is pending. Until a license is added, the repository should be treated as all-rights-reserved.