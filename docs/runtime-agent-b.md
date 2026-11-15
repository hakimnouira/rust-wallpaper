# Runtime intelligence, playback policy & Windows events (Agent B)

Branch `agent/claude-runtime`, based on `c5589324497ff56b7eaae47c2d8b1e6b88061246`.

## 1. Architecture: the runtime state machine

One rule: **events and detectors change facts and reasons; only `reconcile()` touches players.**
`pause_all()` / `resume_all()` are never called by the runtime.

```text
 SystemEventListener ─┐  (session, display power, suspend, topology, Explorer)
 FullscreenTracker  ──┤  (per-monitor foreground kind, debounced)
 Console commands   ──┤  (pause/resume [DISPLAYn], profile, status, quit)
 Player health      ──┘  (missing player => RuntimeError)
          │
          ▼
  RuntimeState (= PlaybackSupervisor)                wallpaper_core::runtime
   ├─ facts: session_locked, display_off, system_suspended
   ├─ PlayerSuspensionManager: global + per-monitor reason sets
   ├─ per-monitor StateDebouncer (fullscreen), pending topology/Explorer work
   └─ reconcile(sink):  desired = !should_suspend(dev)   vs   actual state
          │
          ▼  PlaybackSink (impl for WallpaperManager, policy.rs)
  pause_monitor(dev) / resume_monitor(dev)   only when desired != actual
```

Decision rule per monitor: **play iff the union of global and per-monitor reasons is empty.**
`UserPaused` is a reason like the others, so "the user wants it playing and nothing else
suspends it" is exactly `!should_suspend(dev)`.

| Reason | Scope | Source |
|---|---|---|
| `UserPaused` | global or monitor | console command |
| `FullscreenApplication` | monitor | foreground detector, debounced per monitor |
| `SessionLocked` | global | `WTS_SESSION_LOCK/UNLOCK` |
| `DisplayPoweredOff` | global | `GUID_CONSOLE_DISPLAY_STATE` |
| `SystemSuspended` | global | `PBT_APMSUSPEND` / `PBT_APMRESUME*` |
| `RuntimeError` | monitor | an assigned monitor has no player |

Properties, each covered by a test: removing one reason never clears another; every mutator is
idempotent; duplicate/reordered events converge (facts, not edges); a command that did not take
effect is retried at most every 750 ms, but the *opposite* command is never throttled; players
respawned by Explorer recovery or crash recovery (which start `Playing`) are re-paused on the next
reconcile; an empty monitor enumeration never wipes state.

## 2. Event flow

```text
Win32 thread "sys-event-listener" (hidden top-level window, own message loop)
   WM_WTSSESSION_CHANGE / WM_POWERBROADCAST / WM_DISPLAYCHANGE / TaskbarCreated
        │ drop exact repeats, send small enum over mpsc  (never touches players / MPV IPC)
        ▼
Daemon loop (~60 Hz)
   1 console commands  → RuntimeState::set_user_paused / set_profile
   2 listener.poll()   → apply_event (facts → derived global reasons)
                         Disconnected → restart listener with retry
   3 take_pending_work → topology settled (500 ms quiet): manager.refresh()
                         Explorer settled (1 s, ≥5 s apart): manager.force_recover_explorer()
   4 every poll_interval: FullscreenTracker.probe() → observe_foreground (per-monitor debounce)
   5 every 200 ms: check_and_recover_players(); sync RuntimeError
   6 every 500 ms: manager.refresh()
   7 reconcile on any change, and on a 250 ms heartbeat (retries failed commands)
```

## 3. Policy behaviour

Foreground kinds (ordered): `None < Normal < Maximized < BorderlessFullscreen < ExclusiveFullscreen`.
A profile pauses a monitor when its strongest window is `>=` the profile threshold.

| Profile | Pauses for | Pause delay | Resume delay | Poll |
|---|---|---|---|---|
| ECO | maximized, borderless, exclusive | 1000 ms | 2000 ms | 500 ms |
| BALANCED (default) | borderless, exclusive | 500 ms | 1500 ms | 250 ms |
| PERFORMANCE | exclusive only | 300 ms | 750 ms | 200 ms |

All profiles pause on session lock, display off and system suspend. Resume is slower than pause
so alt-tab or a splash screen cannot make playback oscillate.

| Situation | ECO | BALANCED | PERFORMANCE |
|---|---|---|---|
| Browser/editor maximized (taskbar visible or auto-hidden) | pause | **play** | **play** |
| Borderless-fullscreen game / F11 video | pause | pause | **play** |
| Exclusive D3D fullscreen (per `SHQueryUserNotificationState`) | pause | pause | pause |
| Game on monitor 2, user works on monitor 1 | only monitor 2 pauses | only monitor 2 | only monitor 2 |

Behaviour changes versus the starting commit (deliberate):
* **PERFORMANCE** previously never paused for any fullscreen app although its docs said it would
  for exclusive fullscreen. It now pauses for exclusive fullscreen only.
* **ECO** claimed to pause on maximized windows but could not detect them. It now does.
* Fullscreen suspension is per monitor, not global.

## 4. Defects found in the starting commit (fixed)

1. **Wrong display-power GUID.** `GUID_CONSOLE_DISPLAY_STATE` was `{0017f640-…}`; the real value is
   `{6fe69556-704a-47a0-8f24-c28d936fda47}` (checked against the `windows` crate metadata).
   Registering an unknown GUID succeeds silently, so display power events never fired.
2. **Message-only window** could not receive the `WM_DISPLAYCHANGE` / `TaskbarCreated` broadcasts,
   so topology changes and Explorer restarts were never seen. Now a hidden top-level tool window.
3. **No suspend/resume handling** (`SystemSuspended` existed but nothing set it).
4. **Lock/display state leaked into the fullscreen debouncer**, adding a bogus
   `FullscreenApplication` reason after a lock.
5. **No path set or honoured `UserPaused`**; each handler called `pause_all()` / `resume_monitor()`
   on its own, so e.g. unlock could resume a user-paused monitor.
6. **Respawned players were never re-paused** after Explorer recovery / crash recovery.
7. `"source"` class-name match flagged unrelated windows (e.g. `ResourceMonitor…`) as games.

## 5. Files

Changed: `crates/wallpaper-core/src/lib.rs`, `crates/wallpaper-windows/src/{events,fullscreen,policy,lib}.rs`,
`apps/wallpaper-daemon/src/main.rs`.
Added: `crates/wallpaper-core/src/runtime.rs` (isolated support module), this document.
Not touched: `window.rs`, `desktop_host.rs`, `mpv.rs`, `manager.rs`.

## 6. Known limitations

* **Not verified on real Windows.** Verified: pure logic natively (44 tests); Win32 listener and
  probe code compile for `x86_64-pc-windows-gnu` and their 18 tests pass under **Wine 9**, not
  Windows. The full daemon (Progman/WorkerW, MPV, real sleep, lock, monitor hot-plug) was not run.
* Initial lock/suspend state at daemon start is unknown (defaults to unlocked); display state is
  learned from the notification Windows sends on registration.
* Borderless detection relies on stripped window chrome or known engine classes. A game that keeps
  a caption/frame while covering the monitor is treated as maximized; fullscreen browsers that keep
  `WS_THICKFRAME` may also be. Exclusive detection depends on `SHQueryUserNotificationState`.
* Cloaked windows (other virtual desktops) are not filtered; only the foreground window and
  previously-qualifying windows are inspected.
* A freshly respawned player plays for up to one reconcile pass before being paused (see proposal).
* `UserPaused` lives in daemon memory only (not persisted across restarts); there is no IPC yet,
  only console commands.
* Window class and notification registrations are cleaned up on drop; a hard process kill leaves
  cleanup to the OS, as usual.
* On Linux `wallpaper-windows` does not compile because of two ungated lines in `manager.rs`
  (`win.hwnd.0`, `attach_to_host`), identical at the starting commit.

## 7. Proposals for Agent A (not implemented; their files)

1. `WallpaperManager::play_video_on_monitor_paused(...)` or a `start_paused` flag, so respawned
   players never play a frame while suspended.
2. Expose per-monitor health: `player_state(device)` and whether recovery has given up
   (backoff exhausted), so `RuntimeError` can be exact instead of inferred.
3. `#[cfg(windows)]`-gate the two Windows-only lines in `manager.rs` so the workspace tests run on
   non-Windows CI.
4. Report from `force_recover_explorer()` whether the host was actually rebuilt.

## 8. Manual runtime checklist (needs a real Windows desktop)

Run `wallpaper-daemon --profile balanced` and watch the logs; type commands in the console.

1. `pause`, lock (Win+L), unlock → still paused (`status` shows `user_paused`).
2. `resume`; turn the display off/on (or wait for sleep) → paused, then playing.
3. Launch a borderless game on one monitor → only that monitor logs
   `Fullscreen suspension changed active=true` after ~500 ms; close it → resumes after ~1.5 s.
4. Maximize a browser → keeps playing under `balanced`; `profile eco` → pauses.
5. Sleep/wake the PC → `Received PBT_APMSUSPEND` / resume, playback resumes only if no other reason.
6. `taskkill /f /im explorer.exe` then start it → `Received TaskbarCreated`, desktop recovered,
   and a previously paused monitor is re-paused.
7. Plug/unplug a monitor → `Display topology settled`, other monitors unaffected.
8. `pause DISPLAY2` → only DISPLAY2 pauses; lock/unlock leaves it paused.
