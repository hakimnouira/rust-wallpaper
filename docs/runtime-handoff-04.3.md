# Runtime integration — current handoff

Branch: `main` (runtime integration merged)
Current hardening branch: `work/runtime-hardening`

## Current implementation

The daemon uses `wallpaper_core::supervisor::RuntimeSupervisor` as its runtime orchestration path.

### Monitor lifecycle

- The daemon enumerates monitor details and converts them to stable hardware-identity descriptors.
- SQLite-backed user assignments are stored in the `monitor_assignments` table.
- Monitor discovery, periodic topology reconciliation, Explorer recovery, assign and unassign commands are wired through `RuntimeSupervisor`.
- Removing the last assignment stops the player.
- Renamed hardware outputs invalidate old device-name routing keys; ordinary disconnect/reconnect retains the host recovery marker.

### Player health

- `WallpaperManager` implements `WallpaperHost`.
- Reports include playback state, uptime, working-set memory, actual child-process liveness via non-blocking `Child::try_wait`, and manager recovery backoff state.
- Scheduled probes use one MPV IPC request per player (`video-params`) rather than the multi-property telemetry query.
- Probe rounds are still synchronous and can block the daemon loop; moving probes to a bounded background worker is a remaining hardening task.

### Windows event listener

- The listener tracks WTS session, display-power and `TaskbarCreated` registration independently.
- Suspend/resume and display topology are provided by the hidden top-level notification window.
- Failed registrations retry on the listener's own `WM_TIMER` thread with bounded exponential backoff.
- Registration coverage is exposed through `SystemEventListener::registration_report()`.
- A listener can remain alive with partial registration coverage while its retry timer is functioning.
- If the retry timer cannot be created, listener initialization now fails and the daemon's listener restart policy retries the complete startup sequence. This avoids silently accepting a listener that can never retry failed registrations.

### Persistence

- The daemon's SQLite library and monitor assignments are stored in `%LOCALAPPDATA%\RustWallpaper\library.db`, rather than the process working directory.
- Demo wallpaper assets are still resolved relative to the working directory; packaging must define their final installation location.

## Validation

The previously recorded CI run [#43657331309](https://github.com/hakimnouira/rust-wallpaper/actions/runs/43657331309) passed:

- `cargo fmt --all -- --check`
- `cargo test -p wallpaper-core`: 109 passed
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`

That run predates the current hardening changes. The new branch must pass CI independently.

Cross-target compilation is not a substitute for real Windows desktop validation.

## Remaining runtime work

1. Move MPV health probes off the daemon's main loop, with bounded concurrency, per-probe deadlines and stale-result handling.
2. Detect initial Windows session-lock and display-power state at startup where supported.
3. Extend fullscreen detection for cloaked windows, virtual desktops and unusual game-window behavior.
4. Add real Windows integration coverage for monitor hotplug, Explorer restart, sleep/wake, lock/unlock, fullscreen games, MPV crash recovery and clean shutdown.
5. Improve structured diagnostics for player recovery and monitor identity changes.

## Application-layer work owned separately

The desktop UI, daemon IPC service, library browsing workflows, settings UI, tray integration, installer and updater are separate application/product work. They should use `wallpaper-protocol` and avoid duplicating runtime ownership.

## Important validation boundary

No claim is made that the complete runtime has been exercised on a real Windows desktop. Confirm the behavior on Windows 10/11 hardware with multiple monitors and the supported MPV/decoder configurations before release.
