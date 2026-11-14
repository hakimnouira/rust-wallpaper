//! Platform-independent runtime decision engine.
//!
//! Everything that decides *whether wallpapers should play* lives here, free of Win32 and
//! MPV details so it can be unit-tested on any host:
//!
//! * [`PlayerSuspensionManager`] — authoritative set of independent suspension reasons,
//!   global and per monitor.
//! * [`StateDebouncer`] — asymmetric, clock-injected debounce to stop pause/resume flapping.
//! * [`classify_foreground`] — turns raw window facts into a [`ForegroundKind`].
//! * [`RuntimeState`] — the state machine: events and detectors only change *facts and
//!   reasons*; nothing else may pause or resume a player.
//! * [`reconcile`](RuntimeState::reconcile) — the single mechanism that compares the desired
//!   state with the real player state and issues the minimum pause/resume commands.
//!
//! Every time-dependent method takes `now: Instant` explicitly so tests never need `sleep`.

use crate::{
    ForegroundKind, PerformanceProfile, PlaybackPolicy, SuspensionFlags, SuspensionReason,
};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------------------
// Window geometry & classification
// ---------------------------------------------------------------------------------------

/// Screen rectangle in virtual-desktop pixels (same convention as Win32 `RECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    /// True when `self` covers `other` allowing `tolerance` pixels of shortfall per edge.
    /// Overshoot is always fine: DWM frames routinely extend a few pixels past the monitor.
    pub fn covers(&self, other: &Rect, tolerance: i32) -> bool {
        self.left <= other.left + tolerance
            && self.top <= other.top + tolerance
            && self.right >= other.right - tolerance
            && self.bottom >= other.bottom - tolerance
    }
}

/// Pixels of shortfall still accepted as "covers the whole monitor".
pub const FULLSCREEN_TOLERANCE_PX: i32 = 2;
/// Shortfall accepted when deciding a window fills the monitor's work area (taskbar excluded).
pub const MAXIMIZED_TOLERANCE_PX: i32 = 10;

/// Evaluates geometric coverage between a window rect and a monitor rect.
#[allow(clippy::too_many_arguments)] // kept for compatibility with the original public signature
pub fn is_rect_fullscreen(
    win_left: i32,
    win_top: i32,
    win_right: i32,
    win_bottom: i32,
    mon_left: i32,
    mon_top: i32,
    mon_right: i32,
    mon_bottom: i32,
) -> bool {
    Rect::new(win_left, win_top, win_right, win_bottom).covers(
        &Rect::new(mon_left, mon_top, mon_right, mon_bottom),
        FULLSCREEN_TOLERANCE_PX,
    )
}

/// Window classes that belong to the shell or to us and must never count as "an app".
const SHELL_CLASSES: &[&str] = &[
    "progman",
    "workerw",
    "workergw",
    "shell_traywnd",
    "shell_secondarytraywnd",
    "windows.ui.core.corewindow",
    "multitaskingviewframe",
    "taskswitcherwnd",
    "xamlexplorerhostislandwindow",
    "foregroundstaging",
    "rustwallpaperwindow",
    "rustwallpapernotificationwindow",
];

/// Shell-owned processes whose full-screen-sized overlays are not applications.
/// `explorer.exe` is deliberately absent: File Explorer windows are legitimate apps.
const SHELL_PROCESSES: &[&str] = &[
    "searchhost.exe",
    "searchapp.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "lockapp.exe",
    "textinputhost.exe",
];

/// Class-name fragments that identify game engines / graphics frameworks.
/// Deliberately specific: a bare "source" would also match e.g. "ResourceMonitor".
const GAME_CLASS_FRAGMENTS: &[&str] = &[
    "unrealwindow",
    "unitywndclass",
    "directx",
    "opengl",
    "valve001",
    "sdl_app",
    "glfw",
    "raylib",
];

/// True for windows owned by the shell or by this program.
pub fn is_shell_window(class_name: &str, window_title: &str, process_name: &str) -> bool {
    let class = class_name.to_ascii_lowercase();
    let title = window_title.to_ascii_lowercase();
    let process = process_name.to_ascii_lowercase();

    SHELL_CLASSES.contains(&class.as_str())
        || SHELL_PROCESSES.contains(&process.as_str())
        || (class == "applicationframewindow" && title.contains("search"))
}

/// Heuristically evaluates whether a monitor-covering window is a game / immersive app
/// rather than a maximized productivity application or shell window.
pub fn is_game_heuristic(
    class_name: &str,
    window_title: &str,
    has_caption: bool,
    has_thick_frame: bool,
) -> bool {
    if is_shell_window(class_name, window_title, "") {
        return false;
    }

    let class = class_name.to_ascii_lowercase();
    if GAME_CLASS_FRAGMENTS.iter().any(|f| class.contains(f)) {
        return true;
    }

    // Borderless / exclusive fullscreen games almost always strip WS_CAPTION and WS_THICKFRAME.
    !has_caption && !has_thick_frame
}

/// Raw facts about one top-level window, gathered by the platform layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowFacts {
    pub class_name: String,
    pub title: String,
    /// Lower-cased image file name (e.g. `chrome.exe`), empty if unknown.
    pub process_name: String,
    pub is_zoomed: bool,
    pub has_caption: bool,
    pub has_thick_frame: bool,
    pub window_rect: Rect,
    pub monitor_rect: Rect,
    /// Monitor work area (monitor minus taskbar).
    pub work_rect: Rect,
    /// The shell reports an exclusive / Direct3D fullscreen application is running.
    pub exclusive_fullscreen_hint: bool,
}

/// Classifies a window. Maximized productivity apps are *not* games:
/// they keep their chrome, so they land in [`ForegroundKind::Maximized`] even when an
/// auto-hidden taskbar lets them cover the entire monitor.
pub fn classify_foreground(facts: &WindowFacts) -> ForegroundKind {
    if is_shell_window(&facts.class_name, &facts.title, &facts.process_name) {
        return ForegroundKind::None;
    }
    if facts.exclusive_fullscreen_hint {
        return ForegroundKind::ExclusiveFullscreen;
    }
    if facts.window_rect.is_empty() || facts.monitor_rect.is_empty() {
        return ForegroundKind::None;
    }

    if facts
        .window_rect
        .covers(&facts.monitor_rect, FULLSCREEN_TOLERANCE_PX)
    {
        return if is_game_heuristic(
            &facts.class_name,
            &facts.title,
            facts.has_caption,
            facts.has_thick_frame,
        ) {
            ForegroundKind::BorderlessFullscreen
        } else {
            ForegroundKind::Maximized
        };
    }

    if facts.is_zoomed
        || (!facts.work_rect.is_empty()
            && facts
                .window_rect
                .covers(&facts.work_rect, MAXIMIZED_TOLERANCE_PX))
    {
        return ForegroundKind::Maximized;
    }

    ForegroundKind::Normal
}

/// The strongest window found on one monitor by a detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundObservation {
    /// Monitor device name, e.g. `\\.\DISPLAY1`.
    pub device: String,
    pub kind: ForegroundKind,
    /// Human-readable culprit for logs (`chrome.exe "Title" [Chrome_WidgetWin_1]`).
    pub description: String,
}

// ---------------------------------------------------------------------------------------
// Suspension state
// ---------------------------------------------------------------------------------------

/// Authoritative record of why each monitor's player is (or is not) suspended.
///
/// Reasons are independent: removing one never clears another. `UserPaused` is a reason
/// like any other, so "the user wants it playing AND nothing else suspends it" is exactly
/// `!should_suspend(device)`. All mutators are idempotent and report whether they changed
/// anything.
#[derive(Debug, Clone, Default)]
pub struct PlayerSuspensionManager {
    monitors: HashMap<String, SuspensionFlags>,
    global_flags: SuspensionFlags,
}

impl PlayerSuspensionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a global reason affecting every monitor. Returns true if newly added.
    pub fn add_global(&mut self, reason: SuspensionReason) -> bool {
        self.global_flags.add(reason)
    }

    /// Removes a global reason. Returns true if it was present.
    pub fn remove_global(&mut self, reason: SuspensionReason) -> bool {
        self.global_flags.remove(reason)
    }

    /// Adds or removes a global reason. Returns true if the state changed.
    pub fn set_global(&mut self, reason: SuspensionReason, active: bool) -> bool {
        if active {
            self.add_global(reason)
        } else {
            self.remove_global(reason)
        }
    }

    /// Adds a per-monitor reason. Returns true if newly added.
    pub fn add_monitor(&mut self, device: &str, reason: SuspensionReason) -> bool {
        self.monitors
            .entry(device.to_string())
            .or_default()
            .add(reason)
    }

    /// Removes a per-monitor reason. Returns true if it was present.
    pub fn remove_monitor(&mut self, device: &str, reason: SuspensionReason) -> bool {
        let Some(flags) = self.monitors.get_mut(device) else {
            return false;
        };
        let removed = flags.remove(reason);
        if !flags.is_suspended() {
            self.monitors.remove(device);
        }
        removed
    }

    /// Adds or removes a per-monitor reason. Returns true if the state changed.
    pub fn set_monitor(&mut self, device: &str, reason: SuspensionReason, active: bool) -> bool {
        if active {
            self.add_monitor(device, reason)
        } else {
            self.remove_monitor(device, reason)
        }
    }

    /// True if a monitor must be suspended (any global or per-monitor reason active).
    pub fn should_suspend(&self, device: &str) -> bool {
        self.global_flags.is_suspended()
            || self
                .monitors
                .get(device)
                .is_some_and(SuspensionFlags::is_suspended)
    }

    /// True if `reason` applies to `device`, globally or per monitor.
    pub fn has_reason(&self, device: &str, reason: SuspensionReason) -> bool {
        self.global_flags.contains(reason)
            || self
                .monitors
                .get(device)
                .is_some_and(|flags| flags.contains(reason))
    }

    /// The user's own desired state for this monitor: true if they paused it (or everything).
    pub fn is_user_paused(&self, device: &str) -> bool {
        self.has_reason(device, SuspensionReason::UserPaused)
    }

    /// All reasons for a monitor (global plus per-monitor) in fixed order, without duplicates.
    pub fn active_reasons_for(&self, device: &str) -> Vec<SuspensionReason> {
        SuspensionReason::ALL
            .into_iter()
            .filter(|reason| self.has_reason(device, *reason))
            .collect()
    }

    /// Reasons that apply to every monitor.
    pub fn global_reasons(&self) -> Vec<SuspensionReason> {
        self.global_flags.active_reasons()
    }

    /// Devices that currently carry `reason` as a per-monitor reason.
    pub fn monitors_with(&self, reason: SuspensionReason) -> Vec<String> {
        let mut devices: Vec<String> = self
            .monitors
            .iter()
            .filter(|(_, flags)| flags.contains(reason))
            .map(|(device, _)| device.clone())
            .collect();
        devices.sort();
        devices
    }

    /// Removes a per-monitor reason from every monitor. Returns true if anything changed.
    pub fn remove_monitor_reason_everywhere(&mut self, reason: SuspensionReason) -> bool {
        let mut changed = false;
        for device in self.monitors_with(reason) {
            changed |= self.remove_monitor(&device, reason);
        }
        changed
    }
}

// ---------------------------------------------------------------------------------------
// Debouncing
// ---------------------------------------------------------------------------------------

/// Debouncer preventing rapid pause/resume oscillation (launch splash screens, alt-tab, ...).
///
/// A requested state must persist for the engage delay (`false -> true`) or release delay
/// (`true -> false`) before the stable state flips. Flapping back before then resets the
/// timer without any transition.
#[derive(Debug, Clone)]
pub struct StateDebouncer {
    target_state: bool,
    current_state: bool,
    last_change_intent: Instant,
    engage_delay: Duration,
    release_delay: Duration,
}

impl StateDebouncer {
    /// Symmetric debouncer using one delay in both directions.
    pub fn new(initial_state: bool, debounce_duration: Duration) -> Self {
        Self::with_delays(initial_state, debounce_duration, debounce_duration)
    }

    pub fn with_delays(
        initial_state: bool,
        engage_delay: Duration,
        release_delay: Duration,
    ) -> Self {
        Self {
            target_state: initial_state,
            current_state: initial_state,
            last_change_intent: Instant::now(),
            engage_delay,
            release_delay,
        }
    }

    pub fn set_debounce_duration(&mut self, dur: Duration) {
        self.set_delays(dur, dur);
    }

    pub fn set_delays(&mut self, engage_delay: Duration, release_delay: Duration) {
        self.engage_delay = engage_delay;
        self.release_delay = release_delay;
    }

    /// Submits the requested state at `now`. Returns `Some(new_state)` exactly once when the
    /// stable state flips, otherwise `None`.
    pub fn update_at(&mut self, requested_state: bool, now: Instant) -> Option<bool> {
        if requested_state != self.target_state {
            self.target_state = requested_state;
            self.last_change_intent = now;
        }

        if self.target_state != self.current_state {
            let delay = if self.target_state {
                self.engage_delay
            } else {
                self.release_delay
            };
            if now.saturating_duration_since(self.last_change_intent) >= delay {
                self.current_state = self.target_state;
                return Some(self.current_state);
            }
        }
        None
    }

    /// [`update_at`](Self::update_at) using the wall clock.
    pub fn update(&mut self, requested_state: bool) -> Option<bool> {
        self.update_at(requested_state, Instant::now())
    }

    pub fn current_state(&self) -> bool {
        self.current_state
    }

    /// True while a requested change is waiting out its delay.
    pub fn is_pending(&self) -> bool {
        self.target_state != self.current_state
    }
}

// ---------------------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------------------

/// What a player is actually doing, as far as the daemon can tell without blocking on IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActualPlayback {
    Playing,
    Paused,
    /// Starting, stopped, failed or otherwise not something to command right now.
    Unavailable,
}

/// A command the reconciler wants executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommand {
    Pause,
    Resume,
}

/// The only seam between the decision engine and the real players.
///
/// Implementations must not decide anything: they report state and carry out commands.
pub trait PlaybackSink {
    /// Last known state of each player, keyed by monitor device name. Must not block on IPC.
    fn player_states(&self) -> Vec<(String, ActualPlayback)>;
    fn pause_player(&mut self, device: &str);
    fn resume_player(&mut self, device: &str);
}

/// Minimum spacing between retries of a command that did not take effect.
pub const DEFAULT_COMMAND_RETRY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone)]
struct Reconciler {
    retry_interval: Duration,
    /// Last command issued per device. Only a *repeat of the same command* is throttled:
    /// the opposite command (pause then resume) reflects a genuine new decision.
    last_attempt: HashMap<String, (Instant, PlayerCommand)>,
}

impl Reconciler {
    fn new(retry_interval: Duration) -> Self {
        Self {
            retry_interval,
            last_attempt: HashMap::new(),
        }
    }

    fn run<S: PlaybackSink>(
        &mut self,
        suspension: &PlayerSuspensionManager,
        sink: &mut S,
        now: Instant,
    ) -> Vec<(String, PlayerCommand)> {
        let mut issued = Vec::new();
        let states = sink.player_states();
        let present: HashSet<&str> = states.iter().map(|(d, _)| d.as_str()).collect();
        self.last_attempt
            .retain(|d, _| present.contains(d.as_str()));

        for (device, actual) in &states {
            let want_suspended = suspension.should_suspend(device);
            let command = match (want_suspended, actual) {
                (true, ActualPlayback::Playing) => PlayerCommand::Pause,
                (false, ActualPlayback::Paused) => PlayerCommand::Resume,
                _ => {
                    // In sync (or not commandable): forget any retry bookkeeping.
                    self.last_attempt.remove(device);
                    continue;
                }
            };

            if let Some((last, last_command)) = self.last_attempt.get(device) {
                if *last_command == command
                    && now.saturating_duration_since(*last) < self.retry_interval
                {
                    continue; // same command had no effect yet; do not hammer IPC
                }
            }
            self.last_attempt.insert(device.clone(), (now, command));

            match command {
                PlayerCommand::Pause => sink.pause_player(device),
                PlayerCommand::Resume => sink.resume_player(device),
            }
            issued.push((device.clone(), command));
        }
        issued
    }
}

// ---------------------------------------------------------------------------------------
// Events & control
// ---------------------------------------------------------------------------------------

/// Platform-neutral system events fed into the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEvent {
    SessionLocked,
    SessionUnlocked,
    DisplayPowerOff,
    DisplayPowerOn,
    SystemSuspend,
    SystemResume,
    DisplayTopologyChanged,
    ExplorerRestarted,
}

/// Quiet period after the last topology event before monitors are re-enumerated.
pub const TOPOLOGY_SETTLE: Duration = Duration::from_millis(500);
/// Delay between Explorer's restart notification and tearing down / rebuilding players,
/// giving the new shell time to create its WorkerW.
pub const EXPLORER_SETTLE: Duration = Duration::from_millis(1000);
/// Minimum spacing between two Explorer recoveries (guards against notification bursts).
pub const EXPLORER_MIN_INTERVAL: Duration = Duration::from_secs(5);

/// Work the daemon must perform on the manager, as decided by the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PendingWork {
    pub refresh_topology: bool,
    pub recover_explorer: bool,
}

/// A command received from the user (CLI, console, future IPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCommand {
    Pause { device: Option<String> },
    Resume { device: Option<String> },
    SetProfile(PerformanceProfile),
    Status,
    Quit,
}

/// Parses a line such as `pause`, `resume DISPLAY2`, `profile eco`, `status`, `quit`.
pub fn parse_control_command(line: &str) -> Result<ControlCommand, String> {
    let mut parts = line.split_whitespace();
    let Some(verb) = parts.next() else {
        return Err("empty command".to_string());
    };
    let arg = parts.next().map(str::to_string);
    if parts.next().is_some() {
        return Err("too many arguments".to_string());
    }

    match verb.to_ascii_lowercase().as_str() {
        "pause" => Ok(ControlCommand::Pause { device: arg }),
        "resume" | "play" => Ok(ControlCommand::Resume { device: arg }),
        "profile" => {
            let name = arg.ok_or("usage: profile <eco|balanced|performance>")?;
            PerformanceProfile::parse(&name)
                .map(ControlCommand::SetProfile)
                .ok_or_else(|| format!("unknown profile '{name}'"))
        }
        "status" => Ok(ControlCommand::Status),
        "quit" | "exit" => Ok(ControlCommand::Quit),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// Resolves user input like `display2` / `\\.\DISPLAY2` to a known monitor device name.
pub fn resolve_device(input: &str, known: &[String]) -> Option<String> {
    let wanted = input.to_ascii_lowercase();
    known
        .iter()
        .find(|k| {
            let k = k.to_ascii_lowercase();
            k == wanted || k.ends_with(&format!("\\{wanted}"))
        })
        .cloned()
}

// ---------------------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct MonitorFullscreen {
    debouncer: StateDebouncer,
    /// Description of the window that most recently satisfied the pause threshold.
    culprit: String,
}

/// One monitor's line in a status report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorStatus {
    pub device: String,
    pub suspended: bool,
    pub reasons: Vec<SuspensionReason>,
}

/// Runtime state machine: single source of truth for "should this monitor play?".
///
/// ```text
///  system events ─┐
///  foreground  ───┼─► facts / reasons (PlayerSuspensionManager) ─► reconcile ─► players
///  user commands ─┤                                                    ▲
///  player health ─┘                                    actual state ────┘
/// ```
///
/// Nothing outside [`reconcile`](Self::reconcile) may pause or resume a player.
pub struct RuntimeState {
    policy: PlaybackPolicy,
    suspension: PlayerSuspensionManager,
    reconciler: Reconciler,

    // Facts reported by the OS. Reasons are *derived* from facts + policy, which makes
    // repeated, reordered or duplicated events harmless.
    session_locked: bool,
    display_off: bool,
    system_suspended: bool,

    fullscreen: HashMap<String, MonitorFullscreen>,

    topology_event_at: Option<Instant>,
    explorer_event_at: Option<Instant>,
    last_explorer_recovery: Option<Instant>,
}

impl RuntimeState {
    pub fn new(policy: PlaybackPolicy) -> Self {
        Self {
            policy,
            suspension: PlayerSuspensionManager::new(),
            reconciler: Reconciler::new(DEFAULT_COMMAND_RETRY),
            session_locked: false,
            display_off: false,
            system_suspended: false,
            fullscreen: HashMap::new(),
            topology_event_at: None,
            explorer_event_at: None,
            last_explorer_recovery: None,
        }
    }

    pub fn policy(&self) -> &PlaybackPolicy {
        &self.policy
    }

    pub fn suspension(&self) -> &PlayerSuspensionManager {
        &self.suspension
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.policy.poll_interval_millis)
    }

    /// Switches performance profile. Fullscreen debouncers are retuned in place so a pending
    /// transition keeps its timer; system-event reasons are re-derived under the new policy.
    pub fn set_profile(&mut self, profile: PerformanceProfile) {
        self.policy = PlaybackPolicy::for_profile(profile);
        let (engage, release) = self.debounce_delays();
        for entry in self.fullscreen.values_mut() {
            entry.debouncer.set_delays(engage, release);
        }
        self.sync_system_reasons();
        info!(?profile, "Switched runtime performance profile");
    }

    fn debounce_delays(&self) -> (Duration, Duration) {
        (
            Duration::from_millis(self.policy.pause_debounce_millis),
            Duration::from_millis(self.policy.resume_debounce_millis),
        )
    }

    // ---- system events --------------------------------------------------------------

    /// Applies one event. Facts are idempotent; topology/Explorer events are recorded as
    /// pending work that [`take_pending_work`](Self::take_pending_work) later hands out once.
    pub fn apply_event(&mut self, event: RuntimeEvent, now: Instant) {
        match event {
            RuntimeEvent::SessionLocked => self.session_locked = true,
            RuntimeEvent::SessionUnlocked => self.session_locked = false,
            RuntimeEvent::DisplayPowerOff => self.display_off = true,
            RuntimeEvent::DisplayPowerOn => self.display_off = false,
            RuntimeEvent::SystemSuspend => self.system_suspended = true,
            RuntimeEvent::SystemResume => self.system_suspended = false,
            RuntimeEvent::DisplayTopologyChanged => self.topology_event_at = Some(now),
            RuntimeEvent::ExplorerRestarted => self.explorer_event_at = Some(now),
        }
        self.sync_system_reasons();
    }

    /// Re-derives the three global system reasons from facts and policy.
    fn sync_system_reasons(&mut self) {
        let derived = [
            (
                SuspensionReason::SessionLocked,
                self.session_locked && self.policy.pause_on_lock,
            ),
            (
                SuspensionReason::DisplayPoweredOff,
                self.display_off && self.policy.pause_on_display_power_off,
            ),
            (
                SuspensionReason::SystemSuspended,
                self.system_suspended && self.policy.pause_on_system_suspend,
            ),
        ];
        for (reason, active) in derived {
            if self.suspension.set_global(reason, active) {
                info!(
                    reason = reason.as_str(),
                    active, "Global suspension reason changed"
                );
            }
        }
    }

    /// Returns manager work that has become due, consuming it. Bursts of topology or Explorer
    /// events collapse into a single refresh / recovery once things have settled.
    pub fn take_pending_work(&mut self, now: Instant) -> PendingWork {
        let mut work = PendingWork::default();

        if let Some(at) = self.topology_event_at {
            if now.saturating_duration_since(at) >= TOPOLOGY_SETTLE {
                self.topology_event_at = None;
                work.refresh_topology = true;
            }
        }

        if let Some(at) = self.explorer_event_at {
            let settled = now.saturating_duration_since(at) >= EXPLORER_SETTLE;
            let spaced = match self.last_explorer_recovery {
                Some(t) => now.saturating_duration_since(t) >= EXPLORER_MIN_INTERVAL,
                None => true,
            };
            if settled && spaced {
                self.explorer_event_at = None;
                self.last_explorer_recovery = Some(now);
                work.recover_explorer = true;
            } else if settled && !spaced {
                // Burst of notifications right after a recovery: drop it, the desktop is fresh.
                debug!("Ignoring Explorer notification received during recovery cool-down");
                self.explorer_event_at = None;
            }
        }
        work
    }

    // ---- foreground detection -------------------------------------------------------

    /// Feeds the latest detector output. `monitors` is the complete current monitor list and
    /// must not be empty (an enumeration failure must not wipe state).
    ///
    /// Each monitor is debounced independently, so a game on one monitor never suspends
    /// another monitor's wallpaper.
    pub fn observe_foreground(
        &mut self,
        monitors: &[String],
        observations: &[ForegroundObservation],
        now: Instant,
    ) {
        if monitors.is_empty() {
            return;
        }
        self.forget_missing_monitors(monitors);

        let (engage, release) = self.debounce_delays();
        for device in monitors {
            let strongest = observations
                .iter()
                .filter(|o| &o.device == device)
                .max_by_key(|o| o.kind);
            let raw_pause = strongest.is_some_and(|o| self.policy.pauses_for(o.kind));

            let entry =
                self.fullscreen
                    .entry(device.clone())
                    .or_insert_with(|| MonitorFullscreen {
                        debouncer: StateDebouncer::with_delays(false, engage, release),
                        culprit: String::new(),
                    });
            if raw_pause {
                if let Some(o) = strongest {
                    entry.culprit = o.description.clone();
                }
            }

            if let Some(active) = entry.debouncer.update_at(raw_pause, now) {
                let culprit = entry.culprit.clone();
                if self.suspension.set_monitor(
                    device,
                    SuspensionReason::FullscreenApplication,
                    active,
                ) {
                    info!(
                        device = %device,
                        active,
                        window = %culprit,
                        profile = ?self.policy.profile,
                        "Fullscreen suspension changed"
                    );
                }
            }
        }
    }

    /// Drops per-monitor transient state for monitors that no longer exist.
    /// `UserPaused` is kept so a monitor that returns is still paused.
    fn forget_missing_monitors(&mut self, monitors: &[String]) {
        let present: HashSet<&str> = monitors.iter().map(String::as_str).collect();
        let gone: Vec<String> = self
            .fullscreen
            .keys()
            .filter(|d| !present.contains(d.as_str()))
            .cloned()
            .collect();
        for device in gone {
            self.fullscreen.remove(&device);
            self.suspension
                .remove_monitor(&device, SuspensionReason::FullscreenApplication);
        }
        for device in self
            .suspension
            .monitors_with(SuspensionReason::RuntimeError)
        {
            if !present.contains(device.as_str()) {
                self.suspension
                    .remove_monitor(&device, SuspensionReason::RuntimeError);
            }
        }
    }

    // ---- user commands --------------------------------------------------------------

    /// Applies a user pause/resume. `device = None` is the global switch.
    /// Returns true if state changed.
    pub fn set_user_paused(&mut self, device: Option<&str>, paused: bool) -> bool {
        let changed = match device {
            Some(device) => {
                self.suspension
                    .set_monitor(device, SuspensionReason::UserPaused, paused)
            }
            None => {
                let mut changed = self
                    .suspension
                    .set_global(SuspensionReason::UserPaused, paused);
                if !paused {
                    // "resume" with no target means everything the user paused.
                    changed |= self
                        .suspension
                        .remove_monitor_reason_everywhere(SuspensionReason::UserPaused);
                }
                changed
            }
        };
        if changed {
            info!(device = ?device, paused, "User pause state changed");
        }
        changed
    }

    // ---- player health --------------------------------------------------------------

    /// Marks monitors whose wallpaper should have a player but has none as `RuntimeError`,
    /// and clears the mark once a player is back. Purely informational for the reconciler
    /// (there is nothing to command), but it keeps the reason set truthful in status output.
    pub fn sync_player_presence(&mut self, assigned: &[String], present: &[String]) {
        let present: HashSet<&str> = present.iter().map(String::as_str).collect();
        for device in assigned {
            let missing = !present.contains(device.as_str());
            if self
                .suspension
                .set_monitor(device, SuspensionReason::RuntimeError, missing)
            {
                if missing {
                    warn!(device = %device, "No player for monitor: marking RuntimeError");
                } else {
                    info!(device = %device, "Player is back: clearing RuntimeError");
                }
            }
        }
    }

    // ---- reconcile & reporting ------------------------------------------------------

    /// The single decision mechanism: make real players match the desired state.
    /// Idempotent; returns the commands issued (empty when already in sync).
    pub fn reconcile<S: PlaybackSink>(
        &mut self,
        sink: &mut S,
        now: Instant,
    ) -> Vec<(String, PlayerCommand)> {
        let issued = self.reconciler.run(&self.suspension, sink, now);
        for (device, command) in &issued {
            info!(
                device = %device,
                ?command,
                reasons = ?self.suspension.active_reasons_for(device).iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                "Reconciling player to desired state"
            );
        }
        issued
    }

    pub fn status(&self, monitors: &[String]) -> Vec<MonitorStatus> {
        monitors
            .iter()
            .map(|device| MonitorStatus {
                device: device.clone(),
                suspended: self.suspension.should_suspend(device),
                reasons: self.suspension.active_reasons_for(device),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PerformanceProfile as Profile;

    const D1: &str = r"\\.\DISPLAY1";
    const D2: &str = r"\\.\DISPLAY2";

    fn monitors() -> Vec<String> {
        vec![D1.to_string(), D2.to_string()]
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    // ---- window classification --------------------------------------------------------

    fn facts(class: &str, caption: bool, thick: bool, win: Rect) -> WindowFacts {
        WindowFacts {
            class_name: class.into(),
            title: "t".into(),
            process_name: "app.exe".into(),
            is_zoomed: false,
            has_caption: caption,
            has_thick_frame: thick,
            window_rect: win,
            monitor_rect: Rect::new(0, 0, 1920, 1080),
            work_rect: Rect::new(0, 0, 1920, 1032),
            exclusive_fullscreen_hint: false,
        }
    }

    #[test]
    fn rect_coverage_with_tolerance() {
        assert!(is_rect_fullscreen(0, 0, 1920, 1080, 0, 0, 1920, 1080));
        assert!(is_rect_fullscreen(-2, -2, 1922, 1082, 0, 0, 1920, 1080));
        assert!(is_rect_fullscreen(0, 0, 1919, 1079, 0, 0, 1920, 1080)); // 1 px short is fine
        assert!(!is_rect_fullscreen(100, 100, 1200, 800, 0, 0, 1920, 1080));
        assert!(!is_rect_fullscreen(0, 0, 1920, 1040, 0, 0, 1920, 1080)); // taskbar visible
    }

    #[test]
    fn maximized_productivity_app_is_not_a_game() {
        // Maximized with visible taskbar: DWM overhang of 8px, bottom stops above the taskbar.
        let mut f = facts(
            "Chrome_WidgetWin_1",
            true,
            true,
            Rect::new(-8, -8, 1928, 1040),
        );
        f.is_zoomed = true;
        assert_eq!(classify_foreground(&f), ForegroundKind::Maximized);

        // Maximized with an auto-hidden taskbar covers the entire monitor but keeps its chrome.
        let mut f = facts("CabinetWClass", true, true, Rect::new(-8, -8, 1928, 1088));
        f.is_zoomed = true;
        assert_eq!(classify_foreground(&f), ForegroundKind::Maximized);
    }

    #[test]
    fn borderless_fullscreen_games_are_detected() {
        let f = facts("TankWindowClass", false, false, Rect::new(0, 0, 1920, 1080));
        assert_eq!(
            classify_foreground(&f),
            ForegroundKind::BorderlessFullscreen
        );
        let f = facts("UnityWndClass", true, true, Rect::new(0, 0, 1920, 1080));
        assert_eq!(
            classify_foreground(&f),
            ForegroundKind::BorderlessFullscreen
        );
    }

    #[test]
    fn exclusive_hint_wins_and_shell_never_counts() {
        let mut f = facts("SomeGame", false, false, Rect::new(0, 0, 1920, 1080));
        f.exclusive_fullscreen_hint = true;
        assert_eq!(classify_foreground(&f), ForegroundKind::ExclusiveFullscreen);

        for class in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "MultitaskingViewFrame",
        ] {
            let f = facts(class, false, false, Rect::new(0, 0, 1920, 1080));
            assert_eq!(classify_foreground(&f), ForegroundKind::None, "{class}");
        }
        let mut f = facts(
            "Windows.UI.Core.CoreWindow",
            false,
            false,
            Rect::new(0, 0, 1920, 1080),
        );
        f.process_name = "StartMenuExperienceHost.exe".into();
        assert_eq!(classify_foreground(&f), ForegroundKind::None);
    }

    #[test]
    fn normal_windows_and_degenerate_rects() {
        let f = facts("Notepad", true, true, Rect::new(100, 100, 1200, 800));
        assert_eq!(classify_foreground(&f), ForegroundKind::Normal);
        let f = facts("Notepad", true, true, Rect::default());
        assert_eq!(classify_foreground(&f), ForegroundKind::None);
    }

    #[test]
    fn resource_monitor_is_not_mistaken_for_a_source_engine_game() {
        // The old heuristic matched any class containing "source".
        assert!(!is_game_heuristic(
            "ResourceMonitorWnd",
            "Resource Monitor",
            true,
            true
        ));
        assert!(is_game_heuristic("Valve001", "Half-Life", true, true));
    }

    // ---- suspension manager -----------------------------------------------------------

    #[test]
    fn reasons_coexist_and_removing_one_never_clears_another() {
        let mut m = PlayerSuspensionManager::new();
        assert!(m.add_global(SuspensionReason::SessionLocked));
        assert!(m.add_monitor(D1, SuspensionReason::UserPaused));
        assert!(m.add_monitor(D1, SuspensionReason::FullscreenApplication));

        assert!(m.remove_global(SuspensionReason::SessionLocked));
        assert!(m.should_suspend(D1));
        assert!(!m.should_suspend(D2));

        assert!(m.remove_monitor(D1, SuspensionReason::FullscreenApplication));
        assert!(m.should_suspend(D1), "UserPaused must survive");
        assert!(m.remove_monitor(D1, SuspensionReason::UserPaused));
        assert!(!m.should_suspend(D1));
    }

    #[test]
    fn mutators_are_idempotent_and_report_changes() {
        let mut m = PlayerSuspensionManager::new();
        assert!(m.add_global(SuspensionReason::DisplayPoweredOff));
        assert!(!m.add_global(SuspensionReason::DisplayPoweredOff));
        assert!(m.remove_global(SuspensionReason::DisplayPoweredOff));
        assert!(!m.remove_global(SuspensionReason::DisplayPoweredOff));
        assert!(!m.remove_monitor("nope", SuspensionReason::UserPaused));
        assert!(m.add_monitor(D1, SuspensionReason::UserPaused));
        assert!(!m.add_monitor(D1, SuspensionReason::UserPaused));
    }

    #[test]
    fn active_reasons_merge_global_and_monitor_without_duplicates() {
        let mut m = PlayerSuspensionManager::new();
        m.add_global(SuspensionReason::UserPaused);
        m.add_monitor(D1, SuspensionReason::UserPaused);
        m.add_monitor(D1, SuspensionReason::RuntimeError);
        m.add_global(SuspensionReason::SessionLocked);
        assert_eq!(
            m.active_reasons_for(D1),
            vec![
                SuspensionReason::UserPaused,
                SuspensionReason::SessionLocked,
                SuspensionReason::RuntimeError
            ]
        );
    }

    // ---- debouncer --------------------------------------------------------------------

    #[test]
    fn debouncer_engages_and_releases_with_independent_delays() {
        let t0 = Instant::now();
        let mut d = StateDebouncer::with_delays(false, ms(500), ms(1500));

        assert_eq!(d.update_at(true, t0), None);
        assert_eq!(d.update_at(true, t0 + ms(499)), None);
        assert_eq!(d.update_at(true, t0 + ms(500)), Some(true));
        assert_eq!(
            d.update_at(true, t0 + ms(900)),
            None,
            "transition fires once"
        );

        assert_eq!(d.update_at(false, t0 + ms(1000)), None);
        assert_eq!(d.update_at(false, t0 + ms(2499)), None);
        assert_eq!(d.update_at(false, t0 + ms(2500)), Some(false));
    }

    #[test]
    fn debouncer_flapping_never_transitions() {
        let t0 = Instant::now();
        let mut d = StateDebouncer::with_delays(false, ms(500), ms(500));
        // Alternate every 200ms for 4 seconds: the condition never holds for 500ms.
        for i in 0..20u64 {
            let requested = i % 2 == 0;
            assert_eq!(d.update_at(requested, t0 + ms(i * 200)), None, "step {i}");
        }
        assert!(!d.current_state());
    }

    // ---- reconciler / state machine fixtures -------------------------------------------

    /// Fake player layer reproducing the real manager's behaviours that matter:
    /// pause/resume change state, IPC can fail, and recovery respawns players *playing*.
    #[derive(Default)]
    struct FakeSink {
        players: HashMap<String, ActualPlayback>,
        fail_commands: bool,
        pause_calls: Vec<String>,
        resume_calls: Vec<String>,
    }

    impl FakeSink {
        fn with(devices: &[&str]) -> Self {
            Self {
                players: devices
                    .iter()
                    .map(|d| (d.to_string(), ActualPlayback::Playing))
                    .collect(),
                ..Default::default()
            }
        }
        fn state(&self, d: &str) -> ActualPlayback {
            self.players[d]
        }
        /// What `force_recover_explorer` / crash recovery do: new players start Playing.
        fn respawn_all(&mut self) {
            for state in self.players.values_mut() {
                *state = ActualPlayback::Playing;
            }
        }
    }

    impl PlaybackSink for FakeSink {
        fn player_states(&self) -> Vec<(String, ActualPlayback)> {
            let mut v: Vec<_> = self.players.iter().map(|(d, s)| (d.clone(), *s)).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        }
        fn pause_player(&mut self, device: &str) {
            self.pause_calls.push(device.to_string());
            if !self.fail_commands {
                self.players
                    .insert(device.to_string(), ActualPlayback::Paused);
            }
        }
        fn resume_player(&mut self, device: &str) {
            self.resume_calls.push(device.to_string());
            if !self.fail_commands {
                self.players
                    .insert(device.to_string(), ActualPlayback::Playing);
            }
        }
    }

    fn obs(device: &str, kind: ForegroundKind) -> ForegroundObservation {
        ForegroundObservation {
            device: device.to_string(),
            kind,
            description: format!("test-window {kind:?}"),
        }
    }

    /// Drives foreground observations on a 100ms cadence until `until` (exclusive), returning
    /// the clock after the last step.
    fn run_foreground(
        rt: &mut RuntimeState,
        sink: &mut FakeSink,
        observations: &[ForegroundObservation],
        mut now: Instant,
        for_ms: u64,
    ) -> Instant {
        let end = now + ms(for_ms);
        while now < end {
            rt.observe_foreground(&monitors(), observations, now);
            rt.reconcile(sink, now);
            now += ms(100);
        }
        now
    }

    fn rt(profile: Profile) -> RuntimeState {
        RuntimeState::new(PlaybackPolicy::for_profile(profile))
    }

    // ---- acceptance scenarios ---------------------------------------------------------

    /// 1-3: user pauses, session locks and unlocks: still paused afterwards.
    #[test]
    fn user_pause_survives_lock_unlock() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);

        rt.set_user_paused(None, true);
        rt.reconcile(&mut sink, t);
        assert_eq!(sink.state(D1), ActualPlayback::Paused);

        rt.apply_event(RuntimeEvent::SessionLocked, t);
        rt.reconcile(&mut sink, t + ms(10));
        rt.apply_event(RuntimeEvent::SessionUnlocked, t + ms(5000));
        rt.reconcile(&mut sink, t + ms(5010));

        assert_eq!(
            sink.state(D1),
            ActualPlayback::Paused,
            "UserPaused must survive unlock"
        );
        assert_eq!(sink.state(D2), ActualPlayback::Paused);
        assert!(
            sink.resume_calls.is_empty(),
            "nothing may have been resumed"
        );

        rt.set_user_paused(None, false);
        rt.reconcile(&mut sink, t + ms(6000));
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
    }

    /// 4: display power off/on while fullscreen suspension is active must not resume.
    #[test]
    fn display_power_restore_does_not_override_fullscreen() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);

        // Game goes borderless-fullscreen on DISPLAY1 and the debounce elapses.
        let t = run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D1, ForegroundKind::BorderlessFullscreen)],
            t,
            700,
        );
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
        assert_eq!(sink.state(D2), ActualPlayback::Playing);

        rt.apply_event(RuntimeEvent::DisplayPowerOff, t);
        rt.reconcile(&mut sink, t);
        assert_eq!(
            sink.state(D2),
            ActualPlayback::Paused,
            "display off pauses everything"
        );

        rt.apply_event(RuntimeEvent::DisplayPowerOn, t + ms(2000));
        rt.reconcile(&mut sink, t + ms(2000));
        assert_eq!(
            sink.state(D1),
            ActualPlayback::Paused,
            "fullscreen still suspends DISPLAY1"
        );
        assert_eq!(sink.state(D2), ActualPlayback::Playing, "DISPLAY2 resumes");
        assert_eq!(
            rt.suspension().active_reasons_for(D1),
            vec![SuspensionReason::FullscreenApplication]
        );
    }

    /// 5: fullscreen game launches and closes (debounced, per monitor).
    #[test]
    fn fullscreen_game_launch_and_close() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);
        let game = [obs(D2, ForegroundKind::BorderlessFullscreen)];

        // 400ms < 500ms pause debounce: still playing.
        let t = run_foreground(&mut rt, &mut sink, &game, t, 400);
        assert_eq!(sink.state(D2), ActualPlayback::Playing);
        // Past the debounce: paused, DISPLAY1 untouched.
        let t = run_foreground(&mut rt, &mut sink, &game, t, 300);
        assert_eq!(sink.state(D2), ActualPlayback::Paused);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);

        // Game closes. 1400ms < 1500ms resume debounce: still paused.
        let t = run_foreground(&mut rt, &mut sink, &[], t, 1400);
        assert_eq!(sink.state(D2), ActualPlayback::Paused);
        let _ = run_foreground(&mut rt, &mut sink, &[], t, 300);
        assert_eq!(sink.state(D2), ActualPlayback::Playing);
    }

    /// 6: maximized productivity app stays usable under the selected policy.
    #[test]
    fn maximized_app_pauses_only_under_eco() {
        for (profile, expect_paused) in [
            (Profile::Eco, true),
            (Profile::Balanced, false),
            (Profile::Performance, false),
        ] {
            let t = Instant::now();
            let mut rt = rt(profile);
            let mut sink = FakeSink::with(&[D1]);
            run_foreground(
                &mut rt,
                &mut sink,
                &[obs(D1, ForegroundKind::Maximized)],
                t,
                3000,
            );
            assert_eq!(
                sink.state(D1) == ActualPlayback::Paused,
                expect_paused,
                "{profile:?}"
            );
        }
    }

    /// Borderless games do not pause Performance; exclusive fullscreen does.
    #[test]
    fn performance_profile_pauses_only_for_exclusive_fullscreen() {
        let t = Instant::now();
        let mut rt = rt(Profile::Performance);
        let mut sink = FakeSink::with(&[D1]);
        let t = run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D1, ForegroundKind::BorderlessFullscreen)],
            t,
            2000,
        );
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
        run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D1, ForegroundKind::ExclusiveFullscreen)],
            t,
            600,
        );
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
    }

    /// 7: system sleep/resume, including a user pause made before sleeping.
    #[test]
    fn system_suspend_and_resume() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);

        rt.apply_event(RuntimeEvent::SystemSuspend, t);
        rt.reconcile(&mut sink, t);
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
        assert!(rt
            .suspension()
            .active_reasons_for(D1)
            .contains(&SuspensionReason::SystemSuspended));

        rt.apply_event(RuntimeEvent::SystemResume, t + ms(3000));
        rt.reconcile(&mut sink, t + ms(3000));
        assert_eq!(sink.state(D1), ActualPlayback::Playing);

        // With a user pause in place, resume from sleep must leave it paused.
        rt.set_user_paused(Some(D1), true);
        rt.apply_event(RuntimeEvent::SystemSuspend, t + ms(4000));
        rt.apply_event(RuntimeEvent::SystemResume, t + ms(8000));
        rt.reconcile(&mut sink, t + ms(8000));
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
        assert_eq!(sink.state(D2), ActualPlayback::Playing);
    }

    /// 8: Explorer restart respawns players in Playing state; reconcile must re-pause them.
    #[test]
    fn explorer_recovery_respawn_is_repaused_when_suspended() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);
        rt.set_user_paused(Some(D1), true);
        rt.reconcile(&mut sink, t);
        assert_eq!(sink.state(D1), ActualPlayback::Paused);

        rt.apply_event(RuntimeEvent::ExplorerRestarted, t + ms(100));
        assert_eq!(
            rt.take_pending_work(t + ms(500)),
            PendingWork::default(),
            "still settling"
        );
        let work = rt.take_pending_work(t + ms(1200));
        assert!(work.recover_explorer);

        // The manager rebuilt everything: both new players start Playing.
        sink.respawn_all();
        rt.reconcile(&mut sink, t + ms(1300));
        assert_eq!(
            sink.state(D1),
            ActualPlayback::Paused,
            "UserPaused re-asserted"
        );
        assert_eq!(sink.state(D2), ActualPlayback::Playing);
    }

    #[test]
    fn explorer_and_topology_bursts_collapse_to_one_unit_of_work() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        for i in 0..5 {
            rt.apply_event(RuntimeEvent::ExplorerRestarted, t + ms(i * 50));
            rt.apply_event(RuntimeEvent::DisplayTopologyChanged, t + ms(i * 50));
        }
        // Events kept arriving until +200ms; nothing is due until things go quiet.
        assert_eq!(rt.take_pending_work(t + ms(600)), PendingWork::default());
        let work = rt.take_pending_work(t + ms(1300));
        assert_eq!(
            work,
            PendingWork {
                refresh_topology: true,
                recover_explorer: true
            }
        );
        assert_eq!(
            rt.take_pending_work(t + ms(2000)),
            PendingWork::default(),
            "consumed once"
        );

        // A second notification right after recovery is absorbed by the cool-down.
        rt.apply_event(RuntimeEvent::ExplorerRestarted, t + ms(1500));
        assert!(!rt.take_pending_work(t + ms(2700)).recover_explorer);
        // ...but a later genuine restart is honoured.
        rt.apply_event(RuntimeEvent::ExplorerRestarted, t + ms(9000));
        assert!(rt.take_pending_work(t + ms(10_100)).recover_explorer);
    }

    /// 9: monitor removed and re-added.
    #[test]
    fn topology_change_forgets_transient_state_but_keeps_user_pause() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);
        rt.set_user_paused(Some(D2), true);
        let t = run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D2, ForegroundKind::BorderlessFullscreen)],
            t,
            700,
        );
        assert_eq!(
            rt.suspension().active_reasons_for(D2),
            vec![
                SuspensionReason::UserPaused,
                SuspensionReason::FullscreenApplication
            ]
        );

        // DISPLAY2 unplugged.
        rt.observe_foreground(&[D1.to_string()], &[], t);
        assert_eq!(
            rt.suspension().active_reasons_for(D2),
            vec![SuspensionReason::UserPaused],
            "stale fullscreen reason dropped, user pause kept"
        );

        // DISPLAY2 re-plugged with no game running: still paused by the user.
        let mut sink = FakeSink::with(&[D1, D2]);
        rt.observe_foreground(&monitors(), &[], t + ms(100));
        rt.reconcile(&mut sink, t + ms(100));
        assert_eq!(sink.state(D2), ActualPlayback::Paused);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
    }

    #[test]
    fn empty_monitor_enumeration_does_not_wipe_state() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);
        let t = run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D1, ForegroundKind::BorderlessFullscreen)],
            t,
            700,
        );
        rt.observe_foreground(&[], &[], t);
        assert!(rt
            .suspension()
            .has_reason(D1, SuspensionReason::FullscreenApplication));
    }

    /// 10: every combination of reasons: playing iff the set is empty, order-independent.
    #[test]
    fn multiple_reasons_coexist_in_any_order() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);

        rt.set_user_paused(Some(D1), true);
        rt.apply_event(RuntimeEvent::SessionLocked, t);
        rt.apply_event(RuntimeEvent::DisplayPowerOff, t);
        rt.apply_event(RuntimeEvent::SystemSuspend, t);
        let t = run_foreground(
            &mut rt,
            &mut sink,
            &[obs(D1, ForegroundKind::BorderlessFullscreen)],
            t,
            700,
        );
        assert_eq!(rt.suspension().active_reasons_for(D1).len(), 5);

        // Clear them one at a time in a scrambled order; player must stay paused until the last.
        type Step = Box<dyn Fn(&mut RuntimeState, Instant)>;
        let steps: Vec<Step> = vec![
            Box::new(|r, t| r.apply_event(RuntimeEvent::DisplayPowerOn, t)),
            Box::new(|r, t| r.apply_event(RuntimeEvent::SystemResume, t)),
            Box::new(|r, _t| {
                r.set_user_paused(Some(D1), false);
            }),
            Box::new(|r, t| r.apply_event(RuntimeEvent::SessionUnlocked, t)),
        ];
        let mut t = t;
        for step in &steps {
            t += ms(100);
            step(&mut rt, t);
            rt.reconcile(&mut sink, t);
            assert_eq!(
                sink.state(D1),
                ActualPlayback::Paused,
                "fullscreen still active"
            );
        }
        // Finally the game closes.
        run_foreground(&mut rt, &mut sink, &[], t, 1700);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
        assert!(rt.suspension().active_reasons_for(D1).is_empty());
    }

    /// 11: one monitor suspended independently.
    #[test]
    fn single_monitor_can_be_suspended_independently() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1, D2]);
        rt.set_user_paused(Some(D2), true);
        rt.reconcile(&mut sink, t);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
        assert_eq!(sink.state(D2), ActualPlayback::Paused);

        // A global lock/unlock cycle must not touch D2's pause nor wrongly resume anything.
        rt.apply_event(RuntimeEvent::SessionLocked, t + ms(10));
        rt.reconcile(&mut sink, t + ms(10));
        rt.apply_event(RuntimeEvent::SessionUnlocked, t + ms(20));
        rt.reconcile(&mut sink, t + ms(20));
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
        assert_eq!(sink.state(D2), ActualPlayback::Paused);
    }

    // ---- idempotency, ordering, robustness --------------------------------------------

    #[test]
    fn repeated_and_reordered_events_are_idempotent() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);

        for _ in 0..5 {
            rt.apply_event(RuntimeEvent::SessionLocked, t);
            rt.apply_event(RuntimeEvent::DisplayPowerOff, t);
        }
        rt.reconcile(&mut sink, t);
        assert_eq!(sink.pause_calls.len(), 1, "one pause despite 10 events");

        // Duplicate unlock + display-on.
        for _ in 0..3 {
            rt.apply_event(RuntimeEvent::SessionUnlocked, t + ms(10));
            rt.apply_event(RuntimeEvent::DisplayPowerOn, t + ms(10));
        }
        rt.reconcile(&mut sink, t + ms(10));
        assert_eq!(sink.resume_calls.len(), 1);

        // Reconciling again with nothing changed issues nothing.
        assert!(rt.reconcile(&mut sink, t + ms(20)).is_empty());
        assert!(rt.reconcile(&mut sink, t + ms(5000)).is_empty());
    }

    #[test]
    fn unlock_without_prior_lock_is_harmless() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);
        rt.apply_event(RuntimeEvent::SessionUnlocked, t);
        rt.apply_event(RuntimeEvent::DisplayPowerOn, t);
        rt.apply_event(RuntimeEvent::SystemResume, t);
        assert!(rt.reconcile(&mut sink, t).is_empty());
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
    }

    #[test]
    fn failed_commands_are_retried_but_throttled() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);
        sink.fail_commands = true;
        rt.set_user_paused(None, true);

        // 100ms ticks for 2 seconds: with a 750ms retry gap that is at most 3 attempts.
        let mut now = t;
        for _ in 0..20 {
            rt.reconcile(&mut sink, now);
            now += ms(100);
        }
        assert!(
            (2..=3).contains(&sink.pause_calls.len()),
            "attempts: {}",
            sink.pause_calls.len()
        );

        // Once the player cooperates the pause lands on the next allowed attempt.
        sink.fail_commands = false;
        rt.reconcile(&mut sink, now + ms(1000));
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
    }

    #[test]
    fn unavailable_players_are_never_commanded() {
        let t = Instant::now();
        let mut rt = rt(Profile::Balanced);
        let mut sink = FakeSink::with(&[D1]);
        sink.players
            .insert(D1.to_string(), ActualPlayback::Unavailable);
        rt.set_user_paused(None, true);
        assert!(rt.reconcile(&mut sink, t).is_empty());
        rt.set_user_paused(None, false);
        assert!(rt.reconcile(&mut sink, t + ms(10)).is_empty());
    }

    #[test]
    fn missing_player_is_marked_runtime_error_then_cleared() {
        let mut rt = rt(Profile::Balanced);
        rt.sync_player_presence(&monitors(), &[D1.to_string()]);
        assert_eq!(
            rt.suspension().active_reasons_for(D2),
            vec![SuspensionReason::RuntimeError]
        );
        assert!(rt.suspension().active_reasons_for(D1).is_empty());
        rt.sync_player_presence(&monitors(), &monitors());
        assert!(rt.suspension().active_reasons_for(D2).is_empty());
    }

    #[test]
    fn disabled_policy_flags_suppress_derived_reasons() {
        let t = Instant::now();
        let mut policy = PlaybackPolicy::for_profile(Profile::Balanced);
        policy.pause_on_lock = false;
        let mut rt = RuntimeState::new(policy);
        rt.apply_event(RuntimeEvent::SessionLocked, t);
        assert!(!rt.suspension().should_suspend(D1));
        rt.apply_event(RuntimeEvent::DisplayPowerOff, t);
        assert!(rt.suspension().should_suspend(D1));
    }

    #[test]
    fn switching_profile_retunes_pending_debounce() {
        let t = Instant::now();
        let mut rt = rt(Profile::Eco); // 1000ms pause debounce
        let mut sink = FakeSink::with(&[D1]);
        let game = [obs(D1, ForegroundKind::BorderlessFullscreen)];
        let t = run_foreground(&mut rt, &mut sink, &game, t, 400);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
        rt.set_profile(Profile::Balanced); // 500ms; borderless still counts
        run_foreground(&mut rt, &mut sink, &game, t, 200);
        assert_eq!(
            sink.state(D1),
            ActualPlayback::Paused,
            "pending timer reused under new, shorter delay"
        );
    }

    #[test]
    fn switching_to_a_less_strict_profile_releases_after_resume_debounce() {
        let t = Instant::now();
        let mut rt = rt(Profile::Eco);
        let mut sink = FakeSink::with(&[D1]);
        let maxed = [obs(D1, ForegroundKind::Maximized)];
        let t = run_foreground(&mut rt, &mut sink, &maxed, t, 1200);
        assert_eq!(sink.state(D1), ActualPlayback::Paused);
        rt.set_profile(Profile::Balanced);
        run_foreground(&mut rt, &mut sink, &maxed, t, 2000);
        assert_eq!(sink.state(D1), ActualPlayback::Playing);
    }

    // ---- control parsing --------------------------------------------------------------

    #[test]
    fn control_commands_parse() {
        assert_eq!(
            parse_control_command("pause"),
            Ok(ControlCommand::Pause { device: None })
        );
        assert_eq!(
            parse_control_command("Resume display2"),
            Ok(ControlCommand::Resume {
                device: Some("display2".into())
            })
        );
        assert_eq!(
            parse_control_command("profile eco"),
            Ok(ControlCommand::SetProfile(Profile::Eco))
        );
        assert_eq!(parse_control_command("status"), Ok(ControlCommand::Status));
        assert_eq!(parse_control_command("quit"), Ok(ControlCommand::Quit));
        assert!(parse_control_command("").is_err());
        assert!(parse_control_command("profile turbo").is_err());
        assert!(parse_control_command("pause a b").is_err());
        assert!(parse_control_command("dance").is_err());
    }

    #[test]
    fn device_names_resolve_loosely() {
        let known = monitors();
        assert_eq!(resolve_device("display2", &known), Some(D2.to_string()));
        assert_eq!(resolve_device(D1, &known), Some(D1.to_string()));
        assert_eq!(resolve_device("display3", &known), None);
        assert_eq!(resolve_device("2", &known), None);
    }

    #[test]
    fn global_resume_clears_per_monitor_user_pauses_too() {
        let mut rt = rt(Profile::Balanced);
        rt.set_user_paused(Some(D1), true);
        rt.set_user_paused(None, true);
        assert!(rt.set_user_paused(None, false));
        assert!(!rt.suspension().should_suspend(D1));
        assert!(!rt.set_user_paused(None, false), "idempotent");
    }
}
