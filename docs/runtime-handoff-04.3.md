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

4. **Event listener registration**
   - The listener owns a shared `RegistrationReport` and `RegistrationTracker`.
   - WTS session, display-power, and `TaskbarCreated` registration are tracked independently.
     Suspend/resume and display topology are marked active when the top-level notifier window exists.
   - Failed sources retry on the listener's own `WM_TIMER` message thread with bounded exponential
     backoff (six total attempts, 500 ms initial delay, capped at eight seconds).
   - Coverage is exposed through `SystemEventListener::registration_report()`; the daemon logs
     changes and includes coverage plus diagnostics in the `status` command.
   - A listener can remain alive with partial coverage while retrying; it never reports full
     coverage until every event source is active.

5. **CI**
   - Added `.github/workflows/ci.yml` to run formatting, workspace tests, and a
     `x86_64-pc-windows-gnu` cross-target check on pushes to this branch and PRs to main.
   - Check the live Actions run before treating the branch as build-verified.

## Validation status

GitHub Actions run [#43656679353](https://github.com/hakimnouira/rust-wallpaper/actions/runs/43656679353)
completed successfully on commit `99b97c26ff20e607293fb31764c7343ef0ed8dd0`:

- `cargo fmt --all -- --check`: passed.
- `cargo test -p wallpaper-core`: **109 passed, 0 failed**.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: passed after installing
  MinGW for the bundled SQLite build. This also type-checks the Windows-target test code.

This proves native core tests and a Windows GNU cross-target type/build check. It does not prove
the daemon's runtime behavior on a real Windows desktop.

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
