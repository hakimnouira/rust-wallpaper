# TASK 04.3 — Runtime integration completion

Working branch: `agent/runtime-completion`
Source branch: `agent/claude-runtime`
Base: `c5589324497ff56b7eaae47c2d8b1e6b88061246`

## Implementation status

The daemon has been migrated to use `wallpaper_core::supervisor::RuntimeSupervisor` as its
single runtime orchestration path. The original Claude branch is unchanged.

Implemented on the working branch:

1. **Monitor lifecycle**
   - Daemon enumerates `MonitorDetails` and converts them to stable hardware-identity descriptors.
   - SQLite-backed user assignments are loaded from the existing `library.db` in a separate
     `monitor_assignments` table.
   - New monitor discovery, periodic topology reconciliation, Explorer recovery, assign and
     unassign commands are wired through `RuntimeSupervisor`.
   - Removing the last assignment stops the player. Renamed hardware outputs invalidate the
     old device-name routing key; ordinary disconnect/reconnect retains the host recovery marker.

2. **Player health**
   - `WallpaperManager` implements `WallpaperHost`.
   - Reports include playback state, uptime, working-set memory, actual child-process liveness
     via non-blocking `Child::try_wait`, and manager recovery backoff state.
   - Scheduled health probes use one MPV IPC request per player (`video-params`) rather than
     the multi-property telemetry query. Probe scheduling remains controlled by
     `RuntimeSupervisor::probe_due`.

3. **Daemon orchestration**
   - Startup, topology sync, foreground observation, health updates, reconciliation, telemetry,
     Explorer recovery, and console commands now use the new supervisor.
   - `assign DISPLAYn <path>` persists a monitor assignment.
   - `unassign DISPLAYn` removes the user choice and restores the default policy.
   - Invalid monitor names are rejected; they are not treated as global pause/resume commands.
   - Defaults use the primary monitor's fallback wallpaper, then mirror that primary choice
     to secondary monitors unless they have explicit user assignments.

4. **Event listener safety**
   - A listener is not reported as started if WTS session registration, display-power
     registration, or the `TaskbarCreated` message registration fails.
   - The daemon retries listener creation every two seconds. This is fail-closed behavior:
     partial registration is not silently presented as complete coverage.
   - The platform-independent `RegistrationTracker` remains available and tested, but its
     per-source `WM_TIMER` retry/report integration is not yet wired into the Win32 listener.
     This is the one remaining implementation gap in the event-registration requirement.

5. **CI**
   - Added `.github/workflows/ci.yml` to run formatting, workspace tests, and a
     `x86_64-pc-windows-gnu` cross-target check on pushes to this branch and PRs to main.
   - Check the live Actions run before treating the branch as build-verified.

## Validation status

Do not infer successful compilation or Windows behavior from the code changes alone.
The GitHub Actions run is the validation source for formatting, native tests, and cross-target
checking. A cross-target `cargo check` is not a substitute for running the daemon on Windows.

Still required on a real Windows machine:
- Start the daemon with two monitors and confirm both assignments render.
- Lock/unlock the session and turn the display off/on.
- Launch/close a borderless fullscreen game on each monitor.
- Pause manually, lock/unlock, and confirm the manual pause remains active.
- Disconnect/reconnect a monitor and confirm its persisted assignment returns.
- Kill an MPV child and confirm recovery followed by the correct suspended/playing state.
- Restart Explorer while paused and confirm playback returns paused.
- Test `assign`, `unassign`, `status`, and clean shutdown.

## Useful commands

```text
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace --target x86_64-pc-windows-gnu
```

## Known limitations

- Event registrations currently retry by recreating the entire listener every two seconds.
  Per-source bounded retries and a public registration coverage report remain future work.
- Player process liveness is sampled with `try_wait`; IPC/video health is sampled separately.
  The scheduled probe is one IPC round-trip per player, but a probe round is still synchronous.
- The Windows integration and real desktop behavior must be verified on the user's Windows system.
- The Antigravity MPV/window lifecycle branch is separate and has not been merged into this branch.
