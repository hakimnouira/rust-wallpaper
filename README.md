# Rust Wallpaper

A Windows-first live wallpaper engine written in Rust, inspired by the user-facing capabilities of [Lively Wallpaper](https://github.com/rocksdanister/lively).

## Current project status

The repository now contains a native wallpaper runtime, an initial daemon IPC service, and the first desktop control-panel implementation. This is still an early development build, not a production-ready Lively replacement.

### Implemented foundation

- Win32 desktop-host discovery and wallpaper window attachment.
- MPV-backed video playback with per-monitor player processes.
- Runtime supervision, monitor lifecycle and persistent assignment support.
- Fullscreen/game-aware suspension policies and Windows event handling.
- Explorer/player recovery and runtime health reporting.
- Versioned JSON named-pipe IPC between the daemon and clients.
- Initial egui desktop UI with library listing/import/removal, monitor controls, and connection recovery.

### Not yet production-ready

- Native Windows validation of the complete daemon/UI workflow.
- Per-user named-pipe ACL and client identity validation.
- Native file picker, thumbnail generation and metadata extraction.
- System tray, Windows startup integration and settings persistence.
- WebView2 wallpapers and interactive scenes.
- Installer, updater, signing and crash diagnostics.

## Workspace

```text
apps/
  wallpaper-daemon/   Runtime coordinator, Windows integration and IPC server
  wallpaper-ui/       Native egui control panel and IPC client
  video-player/       Standalone MPV player entry point
crates/
  wallpaper-core/     Domain types, library and runtime orchestration
  wallpaper-windows/  Win32 desktop, monitor and MPV integration
  wallpaper-protocol/ Versioned daemon/client message contract
```

The daemon is the sole owner of Win32 wallpaper windows, runtime state and MPV processes. The UI communicates through `wallpaper-protocol` and does not access the database or manipulate native wallpaper windows directly.

## Development

Requirements: Rust stable. Windows 10/11 is required to run the native daemon and desktop UI.

On Windows:

```powershell
cargo test --workspace
cargo run -p wallpaper-daemon
cargo run -p wallpaper-ui
```

Start the daemon before the UI. The UI reconnects when the daemon is unavailable or restarts.

On non-Windows hosts, platform-independent crates can be tested and the workspace can be cross-checked for the Windows GNU target.

## Documentation

- `docs/architecture.md` — runtime and platform architecture.
- `docs/architecture-proposal-ui-ipc.md` — desktop application design proposal.
- `docs/ipc-protocol.md` — IPC framing, commands and client behavior.
- `apps/wallpaper-ui/README.md` — desktop UI scope and build instructions.

## License

License selection is pending. Until a license is added, the repository should be treated as all-rights-reserved.
