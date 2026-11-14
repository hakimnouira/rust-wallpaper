# TASK 04.3 handoff — Dynamic Monitor Lifecycle & Runtime Reliability

Branch: `agent/claude-runtime` (never touch `main`). Written because the session may end.
**Status: PARTIAL. The platform-independent core is done and tested; the Windows glue and the
daemon migration are NOT done.** Nothing here has run on real Windows.

## How to resume (5 minutes)

```
git fetch origin && git checkout agent/claude-runtime
cargo fmt --all -- --check
cargo test -p wallpaper-core                              # 108 tests, runs on any OS
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu   # needs mingw + windows-gnu std
```
Sandbox recipe that worked (Linux): Rust >=1.78 (micromamba `rust` + `rust-std-x86_64-pc-windows-gnu`),
`apt install gcc-mingw-w64-x86-64 wine64 xvfb`, `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc-posix`,
run Windows test exes with `xvfb-run -a wine64 <exe>` (headless Wine without Xvfb fails CreateWindowExW).
Do not edit `manager.rs` / `mpv.rs` / `window.rs` / `desktop_host.rs` (Agent A).

## Status per requirement

| # | Requirement | State | Where |
|---|---|---|---|
| 1 | Dynamic monitor registry, add/remove/change, restore, safe defaults | **Core DONE + tested** (not wired into daemon) | `core/src/monitors.rs`, `core/src/supervisor.rs` |
| 1 | Stable monitor identity (Windows may renumber `\\.\DISPLAYn`) | **DONE** (core ids + `enumerate_monitor_details()`; Win32 path compiles, not run) | `monitors.rs::MonitorId`, `windows/src/lib.rs` |
| 1 | Persist user assignments, defaults runtime-only | **DONE** (SQLite, own db file `SqliteAssignmentStore`) | `monitors.rs` |
| 2 | Health model (7 dimensions, Unknown never faked) | **Core DONE + tested** (not wired into daemon) | `core/src/health.rs` |
| 2 | `RuntimeError` from verdicts; Starting/Failed/Stopped not healthy | **DONE** | `RuntimeState::sync_player_health`, `health.rs` |
| 2 | Non-blocking health checks | **Partly**: bounded by capped backoff + adaptive probe scheduler; true non-blocking needs Agent A API | `ProbeScheduler`, `Reconciler` |
| 3 | Event registration status + bounded retries | **Pure tracker DONE + tested; Win32 listener NOT patched** | `core/src/registration.rs` (done); `windows/src/events.rs` (TODO) |
| 4 | Recovery correctness | **DONE in core with fake host** (UserPaused/Fullscreen/Suspend survive recovery, Explorer, bursts) | `supervisor.rs` tests |
| 5 | Tests | Core: 108 pass natively. Windows/Wine tests for new Win32 code: NOT written | see below |
| 6 | Validation run | Partial (see Evidence) | |
| 7 | Push + SHA | See final message / `git log` | |

## Evidence (what actually ran)

* `cargo fmt --all -- --check`: OK.
* `cargo test -p wallpaper-core` (native Linux): **108 passed, 0 failed**.
* `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`: OK (cross-compile check only).
* Wine: the earlier 18 `wallpaper-windows` tests passed under Wine 9 + Xvfb (previous commit `795179d`);
  **not re-run after this task's changes**.
* **Not executed**: `cargo test --workspace` natively (fails at baseline: 2 ungated lines in Agent A's
  `manager.rs`, `win.hwnd.0` and `attach_to_host`); `cargo test --workspace --target x86_64-pc-windows-gnu`
  under Wine; anything on real Windows; the daemon itself.

## Bugs found and fixed this task (all have regression tests)

1. Reconcile throttled a *legitimate* re-pause after a respawn when the previous pause had succeeded
   (player stayed Playing under UserPaused/suspend). Fix: verify effect right after each command.
2. Failed-command evidence leaked from a dead process onto its healthy replacement (un-paused up to 30 s).
   Fix: per-process evidence, reset on replacement / manual restart / vanished-and-returned / probe success.
3. Default policy let the primary monitor adopt a secondary's personal wallpaper. Now: primary uses the
   fallback only; others clone the primary.
4. A hung MPV IPC (1.5 s per command, ~5 commands per telemetry call) could stall the loop; reconcile retries
   now back off to 30 s and probes back off to 30 s.

## Remaining work, in order

### Step 1 — `events.rs` registration status (windows crate; Win32 code, test on Wine)
Design is decided, code not written. In `platform::run_notification_thread`:
* Own a `wallpaper_core::registration::RegistrationTracker` and an `Arc<Mutex<RegistrationReport>>`
  shared with `SystemEventListener` (`registration_report()` getter; non-Windows returns default).
* First attempt for each `EventSource`: `SessionNotifications` -> `WTSRegisterSessionNotification`;
  `DisplayPower` -> `RegisterPowerSettingNotification`; `ExplorerRestart` -> `RegisterWindowMessageW("TaskbarCreated") != 0`;
  `SuspendResume` and `DisplayTopology` -> `mark_active` once the hidden *top-level* window exists.
* On failure `tracker.record_failure(..)`; drive retries with `SetTimer`/`WM_TIMER` on the same window
  (interval ~ `min(250 ms, initial_delay/2)`), `tracker.due(now)`, `KillTimer` when
  `!has_pending_retries()`. Keep the single dedicated thread. Publish the report after every change.
* A failed registration must NOT fail `start()` (only window creation may). Add `ListenerOptions`
  (retry policy + injected failure counters for WTS/power/explorer) with `start_with(options)` for tests.
* Wine/Windows tests (extend `windows_integration_tests`): fail WTS twice then succeed -> coverage goes
  Partial -> Full; fail forever -> `Failed`, coverage stays Partial, diagnostics non-empty; clean drop with
  a retry timer pending.

### Step 2 — `policy.rs` adapters (windows crate)
* `impl WallpaperHost for WallpaperManager`: `start_wallpaper` = `play_video_on_monitor(device, &path, cfg)`;
  map `WallpaperManagerError::MonitorNotFound` -> `StartError::NoWindow` (call `refresh()` once and retry).
  `player_reports()` from `player_statuses()` (`process_alive: Unknown`, `uptime: None`, `recovery: None`).
* Telemetry -> `ProbeResult`: `ipc_responded = width||height||fps||gpu any Some`, `video_initialized = width&&height`.
  `collect_telemetry()` is the ONLY blocking call; gate it with `supervisor.probe_due(now)`.
* Add a `ProcessBackoff` exhaustion/reset test (exists partly in `policy.rs` tests).

### Step 3 — Agent A proposal (do not edit their files)
Write `docs/proposals/agent-a-player-health.md` with a ready patch for `manager.rs`/`mpv.rs`:
`WallpaperManager::player_health(&mut self) -> Vec<...>` returning non-blocking `try_wait` liveness,
child pid/uptime, IPC pipe connected, last IPC success time, backoff attempts + exhausted flag, plus
`play_video_on_monitor_paused`. Verify with `git apply --check` in a scratch worktree. Then switch
`player_reports()` to fill `process_alive`, `uptime`, `recovery`. Also ask them to `#[cfg(windows)]`-gate the 2 lines
that break Linux builds. Until then a dead player with a stale `Playing` state is only caught by probe rounds.

### Step 4 — migrate the daemon loop to `RuntimeSupervisor` (`apps/wallpaper-daemon/src/main.rs`)
Replace the static `assigned` list. Per loop: every poll `enumerate_monitor_details()` ->
`supervisor.sync_topology(descs, &mut manager, now)`; if `supervisor.probe_due(now)`:
`collect_telemetry()` -> `record_probes`; `manager.check_and_recover_players()`;
`supervisor.update_health(&mut manager, now)`; `supervisor.reconcile(&mut manager, now)`.
Startup: build `MonitorRegistry::new(Box::new(SqliteAssignmentStore::open(<next to library.db>/monitor_assignments.db)), Some(sample))`.
Replace the two TODO arms for `Assign`/`Unassign` in `handle_control` with `supervisor.assign_user(..)` /
`lifecycle.unassign_user(..)`. Log `registration_report().coverage()` at start and on change; show it in `status`.
Behaviour change to document: monitors now share the primary's wallpaper by default (old demo alternated by index).

### Step 5 — validation and report
Run the 5 commands from the task; run Windows-target tests under `xvfb-run wine64`; record real output; update
`docs/runtime-agent-b.md`; final report must separate Windows / Wine / cross-compile / not-run, and must not claim
Windows runtime verification (still only the manual checklist in `runtime-agent-b.md` section 8, plus: unplug
a monitor, replug it, `taskkill /f /im mpv.exe`, restart Explorer while paused).

## Known limitations (carry forward)
* A player replaced inside a single manager call (no observable gap, no uptime) keeps stale failure evidence
  until the next probe success or the 30 s retry cap. Fix needs Agent A's uptime/pid.
* `enumerate_monitor_details()` identity changes if a monitor moves to a different port (treated as a new monitor).
* Empty monitor enumeration is treated as a failed query, never "all monitors gone".
* The recovery budget is time-based (45 s) because the manager's backoff state is private.
